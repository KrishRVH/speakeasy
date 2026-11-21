use std::{
    fmt,
    fs::File,
    io::{self, BufReader, Read as _, Seek as _},
    num::NonZeroU32,
    path::Path,
    time::Duration,
};

use anyhow::{bail, ensure};
use speakeasy_platform::ReadDirectory;
use zeroize::Zeroizing;

use super::{found, read_complete, recall};
use crate::audio::MAX_SAMPLE_RATE as MAX_RATE;

pub(super) const MAGIC: &[u8; 8] = b"SPKAUD01";
const MAX_START_MS: u64 = 360_000;
pub(super) const MAX_RECORDS: usize = 8_192;
pub(crate) const MAX_CUES: usize = 4_096;
const LANGUAGE_BYTES: usize = 32;
pub(in crate::show::store) const FILE: &str = "audio.log";
pub(in crate::show::store) const FILE_LIMIT: u64 = 128 << 20;
pub(super) const CHUNK_BYTES: usize = 1 << 20;
const PAYLOAD_LIMIT: usize = CHUNK_BYTES + 1;
const BEGIN: u8 = 1;
const SAMPLES: u8 = 2;
const CUE: u8 = 3;
const END: u8 = 4;

#[derive(Debug)]
pub(in crate::show::store) struct Damaged;

impl fmt::Display for Damaged {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("The kept Show audio is damaged or unsupported.")
    }
}

impl std::error::Error for Damaged {}

pub(crate) struct KeptAudio {
    pub rate: NonZeroU32,
    pub start: Duration,
    pub language: Box<str>,
    pub pcm: Zeroizing<Vec<u8>>,
    pub cues: Vec<(usize, usize)>,
    pub complete: bool,
}

pub(super) enum Record {
    Begin {
        rate: u32,
        start: Duration,
        language: Box<str>,
    },
    Samples(Zeroizing<Vec<u8>>),
    Cue {
        before: usize,
        at: usize,
    },
    End,
}

impl Record {
    pub(super) fn bytes(&self) -> usize {
        match self {
            Self::Samples(bytes) => bytes.len().saturating_add(64),
            Self::Begin { .. } | Self::Cue { .. } | Self::End => 128,
        }
    }

    pub(super) fn encode(&self) -> anyhow::Result<Zeroizing<Vec<u8>>> {
        let mut bytes = Zeroizing::new(vec![0; 4]);
        match self {
            Self::Begin {
                rate,
                start,
                language,
            } => {
                ensure!(
                    (1..=MAX_RATE).contains(rate)
                        && start.as_millis() <= u128::from(MAX_START_MS)
                        && language.len() <= LANGUAGE_BYTES,
                    Damaged
                );
                bytes.push(BEGIN);
                bytes.extend_from_slice(&rate.to_le_bytes());
                bytes.extend_from_slice(&u64::try_from(start.as_millis())?.to_le_bytes());
                bytes.extend_from_slice(language.as_bytes());
            },
            Self::Samples(pcm) => {
                ensure!(
                    !pcm.is_empty() && pcm.len() <= CHUNK_BYTES && pcm.len().is_multiple_of(2),
                    Damaged
                );
                bytes.push(SAMPLES);
                bytes.extend_from_slice(pcm);
            },
            Self::Cue { before, at } => {
                ensure!(
                    before <= at
                        && before.is_multiple_of(2)
                        && at.is_multiple_of(2)
                        && *at <= pcm_limit(MAX_RATE),
                    Damaged
                );
                bytes.push(CUE);
                bytes.extend_from_slice(&u64::try_from(*before)?.to_le_bytes());
                bytes.extend_from_slice(&u64::try_from(*at)?.to_le_bytes());
            },
            Self::End => bytes.push(END),
        }
        let (length, payload) = bytes.split_first_chunk_mut::<4>().ok_or(Damaged)?;
        *length = u32::try_from(payload.len())?.to_le_bytes();
        let checksum = crc32fast::hash(payload);
        bytes.extend_from_slice(&checksum.to_le_bytes());
        Ok(bytes)
    }
}

pub(in crate::show::store) fn read(file: File) -> anyhow::Result<KeptAudio> {
    let bytes = file.metadata()?.len();
    ensure!(bytes <= FILE_LIMIT, Damaged);
    decode(BufReader::new(file), usize::try_from(bytes)?)
}

pub(in crate::show::store) fn available(
    directory: &Path,
    name: &str,
) -> anyhow::Result<Option<File>> {
    available_from(&recall::open_directory(directory)?, name)
}

pub(in crate::show::store) fn available_from(
    directory: &ReadDirectory,
    name: &str,
) -> anyhow::Result<Option<File>> {
    let Some((mut file, metadata)) = found(directory.file(Path::new(name)))? else {
        return Ok(None);
    };
    ensure!(metadata.len() <= FILE_LIMIT, Damaged);
    if let Some(expected) = MAGIC.get(..usize::try_from(metadata.len())?) {
        let mut header = [0; MAGIC.len()];
        let header = header.get_mut(..expected.len()).ok_or(Damaged)?;
        file.read_exact(header)?;
        ensure!(header == expected, Damaged);
        return Ok(None);
    }
    let present = decode_prefix(BufReader::new(&mut file), 0, Samples::First)?.is_some();
    if present {
        file.rewind()?;
        Ok(Some(file))
    } else {
        Ok(None)
    }
}

pub(in crate::show::store) fn ended(file: &mut File) -> anyhow::Result<bool> {
    let end = Record::End.encode()?;
    let header = u64::try_from(MAGIC.len())?;
    let Some(start) = file
        .metadata()?
        .len()
        .checked_sub(u64::try_from(end.len())?)
        .filter(|&start| start >= header)
    else {
        return Ok(false);
    };
    file.seek(io::SeekFrom::Start(start))?;
    let mut last = Zeroizing::new(vec![0; end.len()]);
    file.read_exact(&mut last)?;
    file.rewind()?;
    Ok(last == end)
}

enum Samples {
    First,
    All,
}

fn decode(file: impl io::Read, bytes: usize) -> anyhow::Result<KeptAudio> {
    Ok(decode_prefix(file, bytes, Samples::All)?.ok_or(Damaged)?)
}

fn decode_prefix(
    mut file: impl io::Read,
    bytes: usize,
    samples: Samples,
) -> anyhow::Result<Option<KeptAudio>> {
    let mut magic = [0; 8];
    if !read_complete(&mut file, &mut magic)? {
        return Ok(None);
    }
    ensure!(&magic == MAGIC, Damaged);
    let Some(first) = next(&mut file)? else {
        return Ok(None);
    };
    let Some((&BEGIN, header)) = first.split_first() else {
        bail!(Damaged);
    };
    let (rate, header) = header.split_first_chunk::<4>().ok_or(Damaged)?;
    let (start, language) = header.split_first_chunk::<8>().ok_or(Damaged)?;
    let rate = NonZeroU32::new(u32::from_le_bytes(*rate)).ok_or(Damaged)?;
    let start = u64::from_le_bytes(*start);
    ensure!(
        rate.get() <= MAX_RATE && start <= MAX_START_MS && language.len() <= LANGUAGE_BYTES,
        Damaged
    );
    let language = std::str::from_utf8(language)
        .map_err(|_private_content| Damaged)?
        .into();
    let limit = pcm_limit(rate.get());
    let mut audio = KeptAudio {
        rate,
        start: Duration::from_millis(start),
        language,
        pcm: Zeroizing::new(Vec::with_capacity(bytes.min(limit))),
        cues: Vec::new(),
        complete: false,
    };
    for _ in 1..MAX_RECORDS {
        let Some(record) = next(&mut file)? else {
            break;
        };
        let Some((&kind, data)) = record.split_first() else {
            break;
        };
        match kind {
            SAMPLES
                if !data.is_empty()
                    && data.len().is_multiple_of(2)
                    && audio.pcm.len().saturating_add(data.len()) <= limit =>
            {
                match samples {
                    Samples::First => return Ok(Some(audio)),
                    Samples::All => audio.pcm.extend_from_slice(data),
                }
            },
            CUE if audio.cues.len() < MAX_CUES => {
                let ([before, at], []) = data.as_chunks::<8>() else {
                    break;
                };
                let before = usize::try_from(u64::from_le_bytes(*before))?;
                let at = usize::try_from(u64::from_le_bytes(*at))?;
                if !before.is_multiple_of(2)
                    || !at.is_multiple_of(2)
                    || before > at
                    || at > limit
                    || audio.cues.last().is_some_and(|&(previous, previous_at)| {
                        previous > before || previous_at > at
                    })
                {
                    break;
                }
                audio.cues.push((before, at));
            },
            END if data.is_empty() => {
                audio.complete = true;
                break;
            },
            _ => break,
        }
    }
    audio.cues.retain_mut(|(before, at)| {
        *at = (*at).min(audio.pcm.len());
        *before <= audio.pcm.len()
    });
    Ok(match samples {
        Samples::First => None,
        Samples::All => Some(audio),
    })
}

fn next(file: &mut impl io::Read) -> anyhow::Result<Option<Zeroizing<Vec<u8>>>> {
    let mut length = [0; 4];
    if !read_complete(file, &mut length)? {
        return Ok(None);
    }
    let length = usize::try_from(u32::from_le_bytes(length))?;
    if !(1..=PAYLOAD_LIMIT).contains(&length) {
        return Ok(None);
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    let mut checksum = [0; 4];
    if !read_complete(file, &mut bytes)?
        || !read_complete(file, &mut checksum)?
        || crc32fast::hash(&bytes) != u32::from_le_bytes(checksum)
    {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn pcm_limit(rate: u32) -> usize {
    usize::try_from(rate)
        .unwrap_or(usize::MAX)
        .saturating_mul(
            usize::try_from(speakeasy_core::gesture::RECORDING_LIMIT.as_secs())
                .unwrap_or(usize::MAX),
        )
        .saturating_mul(2)
}

#[cfg(test)]
mod tests;
