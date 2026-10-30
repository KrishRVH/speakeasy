//! Bounded PCM checkpoints. Complete checksummed records survive a torn tail; speech and cue
//! offsets stay private and are interpreted only by the audio module during recovery.

use std::{
    fs::File,
    io::{self, BufReader},
    time::Duration,
};

use anyhow::{Context as _, ensure};
use zeroize::Zeroizing;

use crate::audio::MAX_SAMPLE_RATE as MAX_RATE;

pub(super) const MAGIC: &[u8; 8] = b"SPKAUD01";
const MAX_START_MS: u64 = 360_000;
pub(super) const MAX_RECORDS: usize = 8_192;
pub(crate) const MAX_CUES: usize = 4_096;
const LANGUAGE_BYTES: usize = 32;
pub(super) const FILE: &str = "audio.log";
pub(super) const FILE_LIMIT: u64 = 128 << 20;
pub(super) const CHUNK_BYTES: usize = 1 << 20;
const RECORD_LIMIT: usize = CHUNK_BYTES + 9;
const DAMAGED: &str =
    "The kept Show audio is damaged or unsupported. Its frames can still be recovered.";

pub(crate) struct KeptAudio {
    pub rate: u32,
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
                    DAMAGED
                );
                bytes.push(1);
                bytes.extend_from_slice(&rate.to_le_bytes());
                bytes.extend_from_slice(&u64::try_from(start.as_millis())?.to_le_bytes());
                bytes.extend_from_slice(language.as_bytes());
            },
            Self::Samples(pcm) => {
                ensure!(
                    !pcm.is_empty() && pcm.len() <= CHUNK_BYTES && pcm.len().is_multiple_of(2),
                    DAMAGED
                );
                bytes.push(2);
                bytes.extend_from_slice(pcm);
            },
            Self::Cue { before, at } => {
                ensure!(
                    before <= at
                        && before.is_multiple_of(2)
                        && at.is_multiple_of(2)
                        && *at <= pcm_limit(MAX_RATE),
                    DAMAGED
                );
                bytes.push(3);
                bytes.extend_from_slice(&u64::try_from(*before)?.to_le_bytes());
                bytes.extend_from_slice(&u64::try_from(*at)?.to_le_bytes());
            },
            Self::End => bytes.push(4),
        }
        let length = u32::try_from(bytes.len().saturating_sub(4))?;
        bytes
            .get_mut(..4)
            .context(DAMAGED)?
            .copy_from_slice(&length.to_le_bytes());
        let checksum = crc32fast::hash(bytes.get(4..).context(DAMAGED)?);
        bytes.extend_from_slice(&checksum.to_le_bytes());
        Ok(bytes)
    }
}

pub(super) fn read(file: File) -> anyhow::Result<KeptAudio> {
    let bytes = file.metadata()?.len();
    ensure!(bytes <= FILE_LIMIT, DAMAGED);
    decode(BufReader::new(file), usize::try_from(bytes)?)
}

fn decode(mut file: impl io::Read, bytes: usize) -> anyhow::Result<KeptAudio> {
    let mut magic = [0; 8];
    file.read_exact(&mut magic).context(DAMAGED)?;
    ensure!(&magic == MAGIC, DAMAGED);
    let first = next(&mut file)?.context(DAMAGED)?;
    let (kind, mut header) = first.split_first().context(DAMAGED)?;
    ensure!(*kind == 1, DAMAGED);
    let rate = u32::from_le_bytes(take(&mut header)?);
    let start = u64::from_le_bytes(take(&mut header)?);
    ensure!(
        (1..=MAX_RATE).contains(&rate) && start <= MAX_START_MS && header.len() <= LANGUAGE_BYTES,
        DAMAGED
    );
    let language = std::str::from_utf8(header)
        .map_err(|_private_content| anyhow::anyhow!(DAMAGED))?
        .into();
    let limit = pcm_limit(rate);
    let mut audio = KeptAudio {
        rate,
        start: Duration::from_millis(start),
        language,
        pcm: Zeroizing::new(Vec::with_capacity(bytes.min(limit.saturating_add(44)))),
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
            2 if !data.is_empty()
                && data.len().is_multiple_of(2)
                && audio.pcm.len().saturating_add(data.len()) <= limit =>
            {
                audio.pcm.extend_from_slice(data);
            },
            3 if data.len() == 16 && audio.cues.len() < MAX_CUES => {
                let mut data = data;
                let before = usize::try_from(u64::from_le_bytes(take(&mut data)?))?;
                let at = usize::try_from(u64::from_le_bytes(take(&mut data)?))?;
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
            4 if data.is_empty() => {
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
    Ok(audio)
}

fn next(file: &mut impl io::Read) -> anyhow::Result<Option<Zeroizing<Vec<u8>>>> {
    let mut length = [0; 4];
    if !complete(file, &mut length)? {
        return Ok(None);
    }
    let length = usize::try_from(u32::from_le_bytes(length))?;
    if !(1..=RECORD_LIMIT.saturating_sub(8)).contains(&length) {
        return Ok(None);
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    let mut checksum = [0; 4];
    if !complete(file, &mut bytes)?
        || !complete(file, &mut checksum)?
        || crc32fast::hash(&bytes) != u32::from_le_bytes(checksum)
    {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn complete(file: &mut impl io::Read, bytes: &mut [u8]) -> io::Result<bool> {
    match file.read_exact(bytes) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

fn take<const N: usize>(bytes: &mut &[u8]) -> anyhow::Result<[u8; N]> {
    let (head, rest) = bytes.split_first_chunk::<N>().context(DAMAGED)?;
    *bytes = rest;
    Ok(*head)
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
