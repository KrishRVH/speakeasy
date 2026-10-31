//! Bounded inspection of silent fragmented AVC movies. Only complete, contiguous fragment/data
//! pairs survive recovery. Container durations and final indexes cannot establish surviving frames.

mod atoms;
mod initialization;

use std::{fmt, ops::Range, time::Duration};

use atoms::{Atom, Fields, child, children, require};
use initialization::Track;

const HEADER_LIMIT: u64 = 1 << 20;
const SAMPLE_LIMIT: u32 = 10_801;
const DURATION_LIMIT: u64 = 360;

/// A content-free failure to inspect a retained movie's container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovieError {
    /// Metadata is malformed or outside the supported format.
    Invalid(&'static str),
    /// Initialization is readable, but no complete first media fragment survives.
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

/// A structurally complete movie prefix, with timing derived from the surviving sample tables.
///
/// This validates container ranges, track configuration and fragment ordering, not AVC image
/// decoding. A native decoder can still reject damaged compressed image bytes.
#[derive(Clone, Debug)]
pub struct MoviePrefix {
    /// Copy exactly this many bytes, starting at zero, to recover the surviving fragments.
    pub bytes: u64,
    /// Encoded width and height, before any player scaling.
    pub size: [u32; 2],
    /// Surviving presentation duration, relative to the movie's first frame at zero.
    pub duration: Duration,
    /// Number of samples in complete fragments, including held images produced by the codec.
    pub frames: u32,
    samples: Vec<Duration>,
    /// A malformed or incomplete fragment follows the surviving prefix.
    pub truncated: bool,
}

impl MoviePrefix {
    /// Maximum accepted container size. Capture adapters may impose smaller writing budgets.
    pub const FILE_LIMIT: u64 = 1 << 30;

    /// Presentation times of complete surviving samples, relative to the movie's zero origin.
    #[must_use]
    pub fn samples(&self) -> &[Duration] {
        &self.samples
    }

    /// Reads bounded metadata ranges from a silent, single-track fragmented AVC MP4. `read` must
    /// return exactly the requested bytes. The returned prefix excludes optional final indexes;
    /// preserving byte offsets allows copying without rewriting fragment tables. Picture decoding
    /// remains the player's responsibility. Reordered presentation timestamps are unsupported.
    ///
    /// # Errors
    /// Fails when initialization is damaged/unsupported, no complete fragment survives, the
    /// format limits are exceeded, or `read` fails. Format errors never contain private bytes.
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
            samples: Vec::new(),
            truncated: false,
        };
        let mut decoded = 0_u64;
        let mut presented = 0_u64;
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
            let fragment = match fragment(&bytes, position, &data, &track, sequence, decoded) {
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
            presented = presented.max(fragment.presented);
            result.frames = frames;
            for at in fragment.times {
                result.samples.push(duration(at, track.timescale)?);
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
        result.duration = duration(presented, track.timescale)?;
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

    /// Incomplete top-level boxes stop recovery without erasing an earlier complete fragment.
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
    presented: u64,
    times: Vec<u64>,
}

fn fragment(
    bytes: &[u8],
    start: u64,
    data: &Atom,
    track: &Track,
    sequence: u32,
    decoded: u64,
) -> Result<Fragment, MovieError> {
    let boxes = children(bytes)?;
    let mut header = Fields::new(child(&boxes, *b"mfhd")?);
    require(
        header.word()? == 0 && header.word()? == sequence && header.empty(),
        "Movie fragment sequence is invalid",
    )?;
    require(
        boxes.iter().filter(|(kind, _)| kind == b"traf").count() == 1,
        "Movie must contain one fragment track",
    )?;
    let parts = children(child(&boxes, *b"traf")?)?;
    let mut header = Fields::new(child(&parts, *b"tfhd")?);
    let flags = header.word()?;
    require(
        flags & !0x0002_003B == 0 && header.word()? == track.id,
        "Movie fragment track is invalid",
    )?;
    let base = if flags & 1 != 0 {
        header.wide()?
    } else {
        require(flags & 0x0002_0000 != 0, "Movie fragment has no data base")?;
        start
    };
    if flags & 2 != 0 {
        require(
            header.word()? == 1,
            "Movie sample description is unsupported",
        )?;
    }
    let duration = if flags & 8 != 0 {
        header.word()?
    } else {
        track.duration
    };
    let size = if flags & 16 != 0 {
        header.word()?
    } else {
        track.sample_size
    };
    let sample_flags = if flags & 32 != 0 {
        header.word()?
    } else {
        track.flags
    };
    // Media Foundation appends a reserved zero word to tfhd even when no flag names it.
    require(
        header.rest().iter().all(|byte| *byte == 0) && header.rest().len() <= 4,
        "Movie fragment header has trailing data",
    )?;
    let mut result = Fragment {
        samples: 0,
        decoded,
        presented: decoded,
        times: Vec::new(),
    };
    if parts.iter().any(|(kind, _)| kind == b"tfdt") {
        let mut time = Fields::new(child(&parts, *b"tfdt")?);
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
    let runs = parts.iter().filter(|(kind, _)| kind == b"trun");
    for (_, bytes) in runs {
        let mut run = Fields::new(bytes);
        let flags = run.word()?;
        require(
            flags & !0x0100_0F05 == 0,
            "Movie sample table flags are unsupported",
        )?;
        let samples = run.word()?;
        require(
            samples > 0 && result.samples.saturating_add(samples) <= SAMPLE_LIMIT,
            "Movie sample table is too large",
        )?;
        if flags & 1 != 0 {
            let offset = i64::from(run.signed()?);
            require(
                base.checked_add_signed(offset) == Some(next),
                "Movie sample bytes are not contiguous",
            )?;
        } else if result.samples == 0 {
            require(base == next, "Movie implicit sample base misses its data")?;
        }
        let first_flags = if flags & 4 != 0 {
            Some(run.word()?)
        } else {
            None
        };
        require(
            first_flags.is_none() || flags & 0x400 == 0,
            "Movie sample flags conflict",
        )?;
        for index in 0..samples {
            let duration = if flags & 0x100 != 0 {
                run.word()?
            } else {
                duration
            };
            let size = if flags & 0x200 != 0 {
                run.word()?
            } else {
                size
            };
            let sample_flags = if flags & 0x400 != 0 {
                run.word()?
            } else if index == 0 {
                first_flags.unwrap_or(sample_flags)
            } else {
                sample_flags
            };
            let offset = if flags & 0x800 == 0 {
                0
            } else if flags & 0x0100_0000 != 0 {
                i64::from(run.signed()?)
            } else {
                i64::from(run.word()?)
            };
            require(offset == 0, "Movie presentation reordering is unsupported")?;
            require(duration > 0 && size > 0, "Movie sample is empty")?;
            if sequence == 1 && result.samples == 0 && index == 0 {
                require(
                    sample_flags & 0x0001_0000 == 0,
                    "Movie starts without a random-access sample",
                )?;
            }
            let at = result
                .decoded
                .checked_add_signed(offset)
                .ok_or(MovieError::Invalid("Movie presentation time is invalid"))?;
            let end = at
                .checked_add(u64::from(duration))
                .ok_or(MovieError::Invalid("Movie presentation time overflows"))?;
            result.decoded = result
                .decoded
                .checked_add(u64::from(duration))
                .ok_or(MovieError::Invalid("Movie decode time overflows"))?;
            require(
                end <= u64::from(track.timescale).saturating_mul(DURATION_LIMIT)
                    && result.decoded <= u64::from(track.timescale).saturating_mul(DURATION_LIMIT),
                "Movie duration exceeds its limit",
            )?;
            result.presented = result.presented.max(end);
            result.times.push(at);
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
