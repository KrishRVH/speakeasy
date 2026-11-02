mod atoms;
mod initialization;
mod normalization;

use std::{fmt, ops::Range, time::Duration};

use atoms::{Atom, Fields, child, children, require};
use initialization::Track;
pub use normalization::MovieNormalization;

const HEADER_LIMIT: u64 = 1 << 20;
const SAMPLE_LIMIT: u32 = 10_801;
const DURATION_LIMIT: u64 = 360;

const NON_SYNC_SAMPLE: u32 = 0x0001_0000;

mod tfhd {
    pub(super) const BASE_OFFSET: u32 = 0x0000_0001;
    pub(super) const DESCRIPTION: u32 = 0x0000_0002;
    pub(super) const DURATION: u32 = 0x0000_0008;
    pub(super) const SIZE: u32 = 0x0000_0010;
    pub(super) const FLAGS: u32 = 0x0000_0020;
    pub(super) const BASE_IS_MOOF: u32 = 0x0002_0000;
    pub(super) const SUPPORTED: u32 =
        BASE_OFFSET | DESCRIPTION | DURATION | SIZE | FLAGS | BASE_IS_MOOF;
}

mod trun {
    pub(super) const OFFSET: u32 = 0x0000_0001;
    pub(super) const FIRST_FLAGS: u32 = 0x0000_0004;
    pub(super) const DURATION: u32 = 0x0000_0100;
    pub(super) const SIZE: u32 = 0x0000_0200;
    pub(super) const FLAGS: u32 = 0x0000_0400;
    pub(super) const COMPOSITION: u32 = 0x0000_0800;
    pub(super) const SIGNED_COMPOSITION: u32 = 0x0100_0000;
    pub(super) const SUPPORTED: u32 =
        OFFSET | FIRST_FLAGS | DURATION | SIZE | FLAGS | COMPOSITION | SIGNED_COMPOSITION;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovieError {
    Invalid(&'static str),
    NoFragment,
}

impl fmt::Display for MovieError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match self {
            Self::Invalid(message) => message,
            Self::NoFragment => "No complete movie fragment survived",
        })
    }
}

impl std::error::Error for MovieError {}

#[derive(Clone, Debug)]
pub struct MoviePrefix {
    pub bytes: u64,
    pub size: [u32; 2],
    pub duration: Duration,
    pub frames: u32,
    pub timescale: u32,
    samples: Vec<MovieSample>,
    normalization: MovieNormalization,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MovieSample {
    pub at: Duration,
    pub ticks: u64,
}

impl MoviePrefix {
    pub const FILE_LIMIT: u64 = 1 << 30;

    #[must_use]
    pub fn samples(&self) -> &[MovieSample] {
        &self.samples
    }

    #[must_use]
    pub const fn normalization(&self) -> &MovieNormalization {
        &self.normalization
    }

    pub fn read<E: From<MovieError>>(
        length: u64,
        read: impl FnMut(Range<u64>) -> Result<Vec<u8>, E>,
    ) -> Result<Self, E> {
        require(
            (16..=Self::FILE_LIMIT).contains(&length),
            "Movie size is invalid",
        )?;
        let mut input = Input { length, read };
        let (track, mut position) = input.initialize()?;
        let mut result = Self {
            bytes: position,
            size: track.size,
            duration: Duration::ZERO,
            frames: 0,
            timescale: track.timescale,
            samples: Vec::new(),
            normalization: MovieNormalization::default(),
            truncated: false,
        };
        let mut decoded = 0_u64;
        let mut sequence = 1_u32;
        while position < length {
            let Some(atom) = input.atom(position, result.frames > 0)? else {
                result.truncated = true;
                break;
            };
            if atom.kind == *b"mfra" {
                result.truncated = atom.end != length;
                break;
            }
            if atom.kind != *b"moof" || atom.end.saturating_sub(position) > HEADER_LIMIT {
                if result.frames == 0 {
                    return Err(MovieError::Invalid("Movie first fragment is unsupported").into());
                }
                result.truncated = true;
                break;
            }
            let Some(data) = input.atom(atom.end, result.frames > 0)? else {
                result.truncated = true;
                break;
            };
            if data.kind != *b"mdat" {
                if result.frames == 0 {
                    return Err(
                        MovieError::Invalid("Movie first fragment has no media data").into(),
                    );
                }
                result.truncated = true;
                break;
            }
            let bytes = input.bytes(atom.payload..atom.end)?;
            let fragment = match fragment(
                &bytes,
                atom.payload,
                position,
                &data,
                &track,
                sequence,
                decoded,
            ) {
                Ok(fragment) => fragment,
                Err(error) => {
                    if result.frames == 0 {
                        return Err(error.into());
                    }
                    result.truncated = true;
                    break;
                },
            };
            let frames = result
                .frames
                .checked_add(fragment.samples)
                .ok_or(MovieError::Invalid("Movie sample count overflows"))?;
            require(frames <= SAMPLE_LIMIT, "Movie has too many samples")?;
            decoded = fragment.decoded;
            result.frames = frames;
            if let Some(flag) = fragment.normalization {
                result.normalization.headers.push(flag);
            }
            for at in fragment.times {
                result.samples.push(MovieSample {
                    at: duration(at, track.timescale)?,
                    ticks: at,
                });
            }
            result.bytes = data.end;
            position = data.end;
            sequence = sequence
                .checked_add(1)
                .ok_or(MovieError::Invalid("Movie fragment sequence overflows"))?;
        }
        if result.frames == 0 {
            return Err(MovieError::NoFragment.into());
        }
        result.duration = duration(decoded, track.timescale)?;
        Ok(result)
    }
}

fn duration(ticks: u64, timescale: u32) -> Result<Duration, MovieError> {
    let nanos = u128::from(ticks)
        .saturating_mul(1_000_000_000)
        .checked_div(u128::from(timescale))
        .ok_or(MovieError::Invalid("Movie timescale is zero"))?;
    Ok(Duration::from_nanos(u64::try_from(nanos).map_err(
        |_too_long| MovieError::Invalid("Movie duration overflows"),
    )?))
}

struct Input<F> {
    length: u64,
    read: F,
}

impl<E: From<MovieError>, F: FnMut(Range<u64>) -> Result<Vec<u8>, E>> Input<F> {
    fn bytes(&mut self, range: Range<u64>) -> Result<Vec<u8>, E> {
        let requested = range
            .end
            .checked_sub(range.start)
            .ok_or(MovieError::Invalid("Movie range is reversed"))?;
        require(
            range.end <= self.length,
            "Movie metadata range exceeds its file",
        )?;
        require(
            requested <= HEADER_LIMIT,
            "Movie metadata exceeds its limit",
        )?;
        let bytes = (self.read)(range)?;
        require(
            u64::try_from(bytes.len()).unwrap_or(u64::MAX) == requested,
            "Movie metadata read is short",
        )?;
        Ok(bytes)
    }

    fn atom(&mut self, start: u64, has_prefix: bool) -> Result<Option<Atom>, E> {
        if self.length.saturating_sub(start) < 8 {
            return Ok(None);
        }
        let bytes = self.bytes(start..start.saturating_add(8))?;
        let mut fields = Fields::new(&bytes);
        let short = fields.word()?;
        let kind = fields.kind()?;
        let (length, header) = if short == 1 {
            if self.length.saturating_sub(start) < 16 {
                return Ok(None);
            }
            let bytes = self.bytes(start.saturating_add(8)..start.saturating_add(16))?;
            (Fields::new(&bytes).wide()?, 16_u64)
        } else {
            (u64::from(short), 8)
        };
        if length < header {
            require(has_prefix, "Movie box length is invalid")?;
            return Ok(None);
        }
        let Some(end) = start.checked_add(length).filter(|end| *end <= self.length) else {
            return Ok(None);
        };
        Ok(Some(Atom {
            kind,
            payload: start.saturating_add(header),
            end,
        }))
    }

    fn initialize(&mut self) -> Result<(Track, u64), E> {
        let ftyp = self
            .atom(0, false)?
            .ok_or(MovieError::Invalid("Movie has no complete file type"))?;
        require(
            ftyp.kind == *b"ftyp" && ftyp.end <= HEADER_LIMIT,
            "Movie file type is invalid",
        )?;
        let bytes = self.bytes(ftyp.payload..ftyp.end)?;
        require(
            bytes.len() >= 8 && bytes.len().is_multiple_of(4),
            "Movie file type is malformed",
        )?;
        require(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .any(|(at, brand)| {
                    at != 1 && matches!(brand, b"isom" | b"iso2" | b"mp41" | b"mp42" | b"avc1")
                }),
            "Movie brand is unsupported",
        )?;
        let mut position = ftyp.end;
        for _ in 0..32 {
            let atom = self
                .atom(position, false)?
                .ok_or(MovieError::Invalid("Movie initialization is incomplete"))?;
            require(
                atom.end <= HEADER_LIMIT,
                "Movie initialization is too large",
            )?;
            if atom.kind == *b"moov" {
                let bytes = self.bytes(atom.payload..atom.end)?;
                return Ok((initialization::read(&bytes)?, atom.end));
            }
            require(
                matches!(&atom.kind, b"free" | b"skip" | b"uuid" | b"pdin"),
                "Unexpected movie initialization box",
            )?;
            position = atom.end;
        }
        Err(MovieError::Invalid("Movie initialization has too many boxes").into())
    }
}

struct Fragment {
    samples: u32,
    decoded: u64,
    times: Vec<u64>,
    normalization: Option<u64>,
}

fn fragment(
    bytes: &[u8],
    payload: u64,
    start: u64,
    data: &Atom,
    track: &Track,
    sequence: u32,
    decoded: u64,
) -> Result<Fragment, MovieError> {
    let boxes = children(bytes)?;
    let mut header = Fields::new(child(&boxes, *b"mfhd")?.bytes);
    require(
        header.word()? == 0 && header.word()? == sequence && header.empty(),
        "Movie fragment sequence is invalid",
    )?;
    require(
        boxes.iter().filter(|part| part.kind == *b"traf").count() == 1,
        "Movie must contain one fragment track",
    )?;
    let track_box = child(&boxes, *b"traf")?;
    let parts = children(track_box.bytes)?;
    let header_box = child(&parts, *b"tfhd")?;
    let mut header = Fields::new(header_box.bytes);
    let flags = header.word()?;
    require(
        flags & !tfhd::SUPPORTED == 0 && header.word()? == track.id,
        "Movie fragment track is invalid",
    )?;
    let base = if flags & tfhd::BASE_OFFSET != 0 {
        header.wide()?
    } else {
        require(
            flags & tfhd::BASE_IS_MOOF != 0,
            "Movie fragment has no data base",
        )?;
        start
    };
    require(
        header
            .word_if(flags & tfhd::DESCRIPTION != 0)?
            .is_none_or(|description| description == 1),
        "Movie sample description is unsupported",
    )?;
    let duration = header
        .word_if(flags & tfhd::DURATION != 0)?
        .unwrap_or(track.duration);
    let size = header
        .word_if(flags & tfhd::SIZE != 0)?
        .unwrap_or(track.sample_size);
    let sample_flags = header
        .word_if(flags & tfhd::FLAGS != 0)?
        .unwrap_or(track.flags);
    require(
        header.rest().iter().all(|byte| *byte == 0) && header.rest().len() <= 4,
        "Movie fragment header has trailing data",
    )?;
    let mut result = Fragment {
        samples: 0,
        decoded,
        times: Vec::new(),
        normalization: (flags == tfhd::BASE_OFFSET && header.rest() == [0; 4]).then_some(
            payload
                .saturating_add(track_box.payload)
                .saturating_add(header_box.payload)
                .saturating_add(3),
        ),
    };
    if parts.iter().any(|part| part.kind == *b"tfdt") {
        let mut time = Fields::new(child(&parts, *b"tfdt")?.bytes);
        let at = match time.word()? {
            0 => u64::from(time.word()?),
            0x0100_0000 => time.wide()?,
            _ => return Err(MovieError::Invalid("Movie decode time is unsupported")),
        };
        require(
            time.empty() && at == decoded,
            "Movie fragments have a decode-time gap",
        )?;
    }
    let mut next = data.payload;
    let runs = parts.iter().filter(|part| part.kind == *b"trun");
    for part in runs {
        let mut run = Fields::new(part.bytes);
        let flags = run.word()?;
        if flags & trun::SIZE == 0 {
            result.normalization = None;
        }
        require(
            flags & !trun::SUPPORTED == 0,
            "Movie sample table flags are unsupported",
        )?;
        let samples = run.word()?;
        require(
            samples > 0 && result.samples.saturating_add(samples) <= SAMPLE_LIMIT,
            "Movie sample table is too large",
        )?;
        if flags & trun::OFFSET != 0 {
            let offset = i64::from(run.signed()?);
            require(
                base.checked_add_signed(offset) == Some(next),
                "Movie sample bytes are not contiguous",
            )?;
        } else if result.samples == 0 {
            require(base == next, "Movie implicit sample base misses its data")?;
        }
        let first_flags = run.word_if(flags & trun::FIRST_FLAGS != 0)?;
        require(
            first_flags.is_none() || flags & trun::FLAGS == 0,
            "Movie sample flags conflict",
        )?;
        for index in 0..samples {
            let duration = run
                .word_if(flags & trun::DURATION != 0)?
                .unwrap_or(duration);
            let size = run.word_if(flags & trun::SIZE != 0)?.unwrap_or(size);
            let sample_flags = match run.word_if(flags & trun::FLAGS != 0)? {
                Some(flags) => flags,
                None if index == 0 => first_flags.unwrap_or(sample_flags),
                None => sample_flags,
            };
            require(
                run.word_if(flags & trun::COMPOSITION != 0)?
                    .is_none_or(|offset| offset == 0),
                "Movie presentation reordering is unsupported",
            )?;
            require(duration > 0 && size > 0, "Movie sample is empty")?;
            if sequence == 1 && result.samples == 0 && index == 0 {
                require(
                    sample_flags & NON_SYNC_SAMPLE == 0,
                    "Movie starts without a random-access sample",
                )?;
            }
            let end = result
                .decoded
                .checked_add(u64::from(duration))
                .ok_or(MovieError::Invalid("Movie decode time overflows"))?;
            require(
                end <= u64::from(track.timescale).saturating_mul(DURATION_LIMIT),
                "Movie duration exceeds its limit",
            )?;
            result.times.push(result.decoded);
            result.decoded = end;
            next = next
                .checked_add(u64::from(size))
                .filter(|end| *end <= data.end)
                .ok_or(MovieError::Invalid("Movie sample exceeds its data box"))?;
        }
        require(run.empty(), "Movie sample table has trailing data")?;
        result.samples = result.samples.saturating_add(samples);
    }
    require(
        result.samples > 0 && next == data.end,
        "Movie fragment data does not match its sample table",
    )?;
    Ok(result)
}

#[cfg(test)]
mod tests;
