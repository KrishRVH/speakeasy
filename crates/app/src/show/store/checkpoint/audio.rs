use std::{
    fs::File,
    io::{self, BufReader},
    num::NonZeroU32,
    path::Path,
    time::Duration,
};

use anyhow::{Context as _, bail, ensure};

use super::{found, read_complete};

pub(super) const MAGIC: &[u8; 8] = b"SPKAUD01";
pub(in crate::show::store) const FILE: &str = "audio.log";
pub(super) const CHUNK_BYTES: usize = 1 << 20;
const PAYLOAD_LIMIT: usize = CHUNK_BYTES + 1;
const BEGIN: u8 = 1;
const SAMPLES: u8 = 2;
const CUE: u8 = 3;
const END: u8 = 4;
const DAMAGED: &str = "The kept Show audio is damaged.";

pub(crate) struct KeptAudio {
    pub rate: NonZeroU32,
    pub start: Duration,
    pub language: Box<str>,
    pub pcm: Vec<u8>,
    pub cues: Vec<(usize, usize)>,
    pub complete: bool,
}

pub(super) enum Record {
    Begin {
        rate: u32,
        start: Duration,
        language: Box<str>,
    },
    Samples(Vec<u8>),
    Cue {
        before: usize,
        at: usize,
    },
    End,
}

impl Record {
    pub(super) fn encode(&self) -> anyhow::Result<Vec<u8>> {
        let mut payload = Vec::new();
        match self {
            Self::Begin {
                rate,
                start,
                language,
            } => {
                payload.push(BEGIN);
                payload.extend_from_slice(&rate.to_le_bytes());
                payload.extend_from_slice(&u64::try_from(start.as_millis())?.to_le_bytes());
                payload.extend_from_slice(language.as_bytes());
            },
            Self::Samples(pcm) => {
                payload.push(SAMPLES);
                payload.extend_from_slice(pcm);
            },
            Self::Cue { before, at } => {
                payload.push(CUE);
                payload.extend_from_slice(&u64::try_from(*before)?.to_le_bytes());
                payload.extend_from_slice(&u64::try_from(*at)?.to_le_bytes());
            },
            Self::End => payload.push(END),
        }
        let mut bytes = u32::try_from(payload.len())?.to_le_bytes().to_vec();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        Ok(bytes)
    }
}

pub(in crate::show::store) fn read(path: &Path) -> anyhow::Result<Option<KeptAudio>> {
    let Some(file) = found(File::open(path))? else {
        return Ok(None);
    };
    decode(BufReader::new(file))
}

fn decode(mut file: impl io::Read) -> anyhow::Result<Option<KeptAudio>> {
    let mut magic = [0; 8];
    if !read_complete(&mut file, &mut magic)? {
        return Ok(None);
    }
    ensure!(&magic == MAGIC, DAMAGED);
    let Some(first) = next(&mut file)? else {
        return Ok(None);
    };
    let Some((&BEGIN, header)) = first.split_first() else {
        bail!(DAMAGED);
    };
    let (rate, header) = header.split_first_chunk::<4>().context(DAMAGED)?;
    let (start, language) = header.split_first_chunk::<8>().context(DAMAGED)?;
    let mut audio = KeptAudio {
        rate: NonZeroU32::new(u32::from_le_bytes(*rate)).context(DAMAGED)?,
        start: Duration::from_millis(u64::from_le_bytes(*start)),
        language: std::str::from_utf8(language).context(DAMAGED)?.into(),
        pcm: Vec::new(),
        cues: Vec::new(),
        complete: false,
    };
    while let Some(record) = next(&mut file)? {
        match record.split_first() {
            Some((&SAMPLES, samples)) => audio.pcm.extend_from_slice(samples),
            Some((&CUE, cue)) => {
                let ([before, at], []) = cue.as_chunks::<8>() else {
                    break;
                };
                audio.cues.push((
                    usize::try_from(u64::from_le_bytes(*before))?,
                    usize::try_from(u64::from_le_bytes(*at))?,
                ));
            },
            Some((&END, [])) => {
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
    Ok((!audio.pcm.is_empty()).then_some(audio))
}

fn next(file: &mut impl io::Read) -> anyhow::Result<Option<Vec<u8>>> {
    let mut length = [0; 4];
    if !read_complete(file, &mut length)? {
        return Ok(None);
    }
    let length = usize::try_from(u32::from_le_bytes(length))?;
    if !(1..=PAYLOAD_LIMIT).contains(&length) {
        return Ok(None);
    }
    let mut bytes = vec![0; length];
    let mut checksum = [0; 4];
    if !read_complete(file, &mut bytes)?
        || !read_complete(file, &mut checksum)?
        || crc32fast::hash(&bytes) != u32::from_le_bytes(checksum)
    {
        return Ok(None);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests;
