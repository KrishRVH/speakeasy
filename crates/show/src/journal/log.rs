//! A take's pure event fold and bounded checkpoint records. Replay uses the live journal's
//! admission, tile updates, and thinning; an interrupted prefix never fabricates an ending.

use super::{
    Entry, Journal, SourceError,
    record::{Cursor, emit_record, payload, require},
    tiles,
};
use crate::{Ending, Image, Moment, Point, Recording, Signature, Size, Span, Tracker};

const MAGIC: &[u8; 8] = b"SPKLOG01";
const DURATION_MS: u64 = 360_000;
const EDGE: usize = super::MAX_FRAME_EDGE as usize;

/// Events consumed by a take, in order. An arrival is logged even when sampling skipped its image.
#[derive(Clone, Debug)]
pub enum TakeEvent {
    /// The logical capture surface changed.
    Surface(Size),
    /// A normalized pointer sample, possibly outside the surface.
    Pointer {
        /// Milliseconds from the session start.
        at: Moment,
        /// Position normalized to the current captured surface.
        point: Point,
        /// Shift is held without taking input away from the focused app.
        drawing: bool,
        /// The primary pointer button is down.
        pressed: bool,
    },
    /// The two Shift taps that asked for a pin.
    Pin(Span),
    /// One capture arrival, with tiles only when an image was admitted and could be prepared.
    Arrival {
        /// Arrival time relative to the session start.
        at: Moment,
        /// Absent when sampling skipped this arrival or preparing its image failed.
        entry: Option<Entry>,
    },
    /// Capture explicitly ended; absent after interruption.
    End {
        /// The final time, no earlier than any consumed event.
        at: Moment,
        /// Whether capture finished normally or its target was lost.
        ending: Ending,
    },
}

impl TakeEvent {
    /// Conservative memory retained while this event waits for a checkpoint writer.
    #[must_use]
    pub fn bytes(&self) -> usize {
        match self {
            Self::Arrival {
                entry: Some(entry), ..
            } => entry.bytes().saturating_add(64),
            _ => 64,
        }
    }
}

/// Journal and pointer state for one Show. No file, clock, callback, or native resource is owned.
#[derive(Debug)]
pub struct Take {
    journal: Journal,
    tracker: Tracker,
    latest: Moment,
    ending: Option<Ending>,
}

impl Take {
    /// Starts a take with the given compressed-frame budget and logical surface.
    #[must_use]
    pub fn new(budget: usize, surface: Size) -> Self {
        Self {
            journal: Journal::with_budget(budget),
            tracker: Tracker::new(surface),
            latest: Moment::START,
            ending: None,
        }
    }

    /// Read-only frame state for preview and publication.
    #[must_use]
    pub const fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Read-only ink and marks for the live overlay.
    #[must_use]
    pub const fn tracker(&self) -> &Tracker {
        &self.tracker
    }

    /// Whether the take consumed an explicit end record.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.ending.is_some()
    }

    /// Consumes a live arrival. `prepare` runs only for admitted arrivals and can reject an invalid
    /// image. The returned event records that exact decision for a checkpoint.
    ///
    /// # Errors
    /// Rejects arrivals after End or outside the checkpoint's time range, before preparing pixels.
    pub fn arrive(
        &mut self,
        at: Moment,
        prepare: impl FnOnce() -> Option<Image>,
    ) -> Result<TakeEvent, SourceError> {
        require(!self.ended(), "checkpoint has events after its ending")?;
        valid_time(at)?;
        self.latest = self.latest.max(at);
        let entry = self
            .journal
            .admit()
            .and_then(|admission| prepare().map(|image| admission.record(at, image)));
        Ok(TakeEvent::Arrival { at, entry })
    }

    /// Ends live capture at the last consumed event or the requested time, within the log's
    /// timeline. Delayed native callbacks and a suspend cannot invalidate frames already captured.
    pub fn end(&mut self, at: Moment, ending: Ending) -> TakeEvent {
        self.latest = self
            .latest
            .max(Moment::from_millis(at.as_millis().min(DURATION_MS)));
        self.ending = Some(ending);
        TakeEvent::End {
            at: self.latest,
            ending,
        }
    }

    /// Applies a normalized event, including a replayed arrival, through the same take state.
    ///
    /// # Errors
    /// Rejects events after End, nonfinite geometry, excessive times, or a frame that sampling
    /// would have skipped. A caller stops replay at the first rejected record.
    pub fn apply(&mut self, event: &TakeEvent) -> Result<(), SourceError> {
        require(!self.ended(), "checkpoint has events after its ending")?;
        match event {
            TakeEvent::Surface(size) => {
                valid_surface(*size)?;
                self.tracker.resize(*size);
            },
            TakeEvent::Pointer {
                at,
                point,
                drawing,
                pressed,
            } => {
                valid_time(*at)?;
                require(point.is_finite(), "checkpoint pointer is not finite")?;
                self.latest = self.latest.max(*at);
                self.tracker.observe(*at, *point, *drawing, *pressed);
            },
            TakeEvent::Pin(span) => {
                valid_time(span.end())?;
                self.latest = self.latest.max(span.end());
                self.tracker.pin(*span);
            },
            TakeEvent::Arrival { at, entry } => {
                valid_time(*at)?;
                // Check the decision before advancing, so an inconsistent record leaves the prefix intact.
                require(
                    entry.is_none() || self.journal.arrived.is_multiple_of(self.journal.stride),
                    "checkpoint frame disagrees with sampling",
                )?;
                if let Some(entry) = entry {
                    require(
                        entry.reset
                            || self.journal.segments.last().is_some_and(|segment| {
                                segment.width == entry.width && segment.height == entry.height
                            }),
                        "checkpoint delta has no preceding frame",
                    )?;
                }
                if let Some(admission) = self.journal.admit()
                    && let Some(entry) = entry
                {
                    admission.restore(*at, entry)?;
                }
                self.latest = self.latest.max(*at);
            },
            TakeEvent::End { at, ending } => {
                valid_time(*at)?;
                require(*at >= self.latest, "checkpoint ending precedes its events")?;
                self.latest = *at;
                self.ending = Some(*ending);
            },
        }
        Ok(())
    }

    /// Finishes the last complete prefix. Missing End means the screen stopped; no narration is
    /// inferred. The journal's last millisecond-adjusted sample is included in its duration.
    #[must_use]
    pub fn finish(self) -> (Recording, Journal) {
        let at = self
            .journal
            .samples()
            .last()
            .map_or(self.latest, |sample| self.latest.max(sample.at));
        let recording = Recording {
            ending: self.ending.unwrap_or(Ending::ScreenStopped),
            ..self.tracker.finish(at)
        };
        (recording, self.journal.sealed())
    }
}

/// Stateless checkpoint encoding. Callers stream one framed record at a time and stop at a torn
/// or invalid record. This codec never allocates for a caller-declared count before bounding it.
#[derive(Debug)]
pub struct Log;

impl Log {
    /// Fixed header length, including its checksum.
    pub const HEADER_BYTES: usize = 33;
    /// Maximum compressed bytes a checkpoint can ask its replay journal to retain.
    pub const MAX_BUDGET: usize = 384 << 20;
    /// Largest complete event accepted before allocation, including framing.
    pub const RECORD_LIMIT: usize = 40 << 20;

    /// Encodes a versioned checkpoint header.
    ///
    /// # Errors
    /// Rejects invalid geometry or a budget above the replay limit.
    pub fn header(budget: usize, surface: Size) -> Result<Vec<u8>, SourceError> {
        require(
            budget <= Self::MAX_BUDGET,
            "checkpoint memory budget is too large",
        )?;
        valid_surface(surface)?;
        let mut data = Vec::new();
        data.extend_from_slice(&u64::try_from(budget).unwrap_or(u64::MAX).to_le_bytes());
        data.extend_from_slice(&surface.width.to_le_bytes());
        data.extend_from_slice(&surface.height.to_le_bytes());
        let mut bytes = MAGIC.to_vec();
        emit_record(0, &data, &mut |chunk: &[u8]| {
            bytes.extend_from_slice(chunk);
            Ok::<_, SourceError>(())
        })?;
        Ok(bytes)
    }

    /// Opens a replay take from exactly one header.
    ///
    /// # Errors
    /// Rejects unsupported versions, truncated headers, bad checksums, or unreasonable limits.
    pub fn start(header: &[u8]) -> Result<Take, SourceError> {
        require(
            header.len() == Self::HEADER_BYTES && header.get(..8) == Some(MAGIC),
            "unsupported or truncated checkpoint header",
        )?;
        let mut data = Cursor(payload(header.get(8..).unwrap_or_default(), 0)?);
        let budget = usize::try_from(data.wide()?)
            .map_err(|_out_of_range| SourceError("checkpoint memory budget is too large"))?;
        require(
            budget <= Self::MAX_BUDGET,
            "checkpoint memory budget is too large",
        )?;
        let surface = Size {
            width: f32::from_le_bytes(data.take()?),
            height: f32::from_le_bytes(data.take()?),
        };
        valid_surface(surface)?;
        Ok(Take::new(budget, surface))
    }

    /// Reads the four-byte record length before the caller allocates the remaining bytes.
    ///
    /// # Errors
    /// Rejects a partial length or a record above the fixed cap.
    pub fn record_size(header: &[u8]) -> Result<usize, SourceError> {
        let length = Cursor(header)
            .usize()?
            .checked_add(9)
            .ok_or(SourceError("checkpoint record is too large"))?;
        require(
            length <= Self::RECORD_LIMIT,
            "checkpoint record is too large",
        )?;
        Ok(length)
    }

    /// Encodes one event, off the recording thread. Tile data is copied once into the bounded
    /// record buffer; callers can reuse the journal's immutable entry on their writer lane.
    ///
    /// # Errors
    /// Rejects frames larger than the checkpoint format permits.
    pub fn encode(event: &TakeEvent) -> Result<Vec<u8>, SourceError> {
        let mut data = vec![0; 5];
        let kind = match event {
            TakeEvent::Surface(size) => {
                data.extend_from_slice(&size.width.to_le_bytes());
                data.extend_from_slice(&size.height.to_le_bytes());
                1
            },
            TakeEvent::Pointer {
                at,
                point,
                drawing,
                pressed,
            } => {
                data.extend_from_slice(&at.as_millis().to_le_bytes());
                data.extend_from_slice(&point.x.to_le_bytes());
                data.extend_from_slice(&point.y.to_le_bytes());
                data.push(u8::from(*drawing));
                data.push(u8::from(*pressed));
                2
            },
            TakeEvent::Pin(span) => {
                data.extend_from_slice(&span.start().as_millis().to_le_bytes());
                data.extend_from_slice(&span.end().as_millis().to_le_bytes());
                3
            },
            TakeEvent::Arrival { at, entry } => {
                data.extend_from_slice(&at.as_millis().to_le_bytes());
                data.push(u8::from(entry.is_some()));
                if let Some(entry) = entry {
                    entry.encode(&mut data)?;
                }
                4
            },
            TakeEvent::End { at, ending } => {
                data.extend_from_slice(&at.as_millis().to_le_bytes());
                data.push(match ending {
                    Ending::Finished => 0,
                    Ending::ScreenStopped => 1,
                    Ending::WindowClosed => 2,
                    Ending::WindowOffScreen => 3,
                });
                5
            },
        };
        require(
            data.len().saturating_add(4) <= Self::RECORD_LIMIT,
            "checkpoint record is too large",
        )?;
        let length = u32::try_from(data.len().saturating_sub(5))
            .map_err(|_out_of_range| SourceError("checkpoint record is too large"))?;
        let prefix = data
            .get_mut(..5)
            .ok_or(SourceError("checkpoint record is truncated"))?;
        let (length_bytes, kind_byte) = prefix.split_at_mut(4);
        length_bytes.copy_from_slice(&length.to_le_bytes());
        kind_byte.copy_from_slice(&[kind]);
        let checksum = crc32fast::hash(data.get(4..).unwrap_or_default());
        data.extend_from_slice(&checksum.to_le_bytes());
        Ok(data)
    }

    /// Decodes one complete checksummed event. Pixel bytes remain compressed.
    ///
    /// # Errors
    /// Rejects malformed records, invalid dimensions/counts, and unknown event kinds.
    pub fn decode(record: &[u8]) -> Result<TakeEvent, SourceError> {
        require(
            record.len() <= Self::RECORD_LIMIT,
            "checkpoint record is too large",
        )?;
        let kind = *record
            .get(4)
            .ok_or(SourceError("checkpoint record is truncated"))?;
        let mut data = Cursor(payload(record, kind)?);
        let event = match kind {
            1 => TakeEvent::Surface(Size {
                width: f32::from_le_bytes(data.take()?),
                height: f32::from_le_bytes(data.take()?),
            }),
            2 => TakeEvent::Pointer {
                at: Moment::from_millis(data.wide()?),
                point: Point::new(
                    f32::from_le_bytes(data.take()?),
                    f32::from_le_bytes(data.take()?),
                ),
                drawing: boolean(&mut data)?,
                pressed: boolean(&mut data)?,
            },
            3 => TakeEvent::Pin(Span::new(
                Moment::from_millis(data.wide()?),
                Moment::from_millis(data.wide()?),
            )),
            4 => TakeEvent::Arrival {
                at: Moment::from_millis(data.wide()?),
                entry: if boolean(&mut data)? {
                    Some(Entry::decode(&mut data)?)
                } else {
                    None
                },
            },
            5 => TakeEvent::End {
                at: Moment::from_millis(data.wide()?),
                ending: match data.take::<1>()? {
                    [0] => Ending::Finished,
                    [1] => Ending::ScreenStopped,
                    [2] => Ending::WindowClosed,
                    [3] => Ending::WindowOffScreen,
                    _ => return Err(SourceError("checkpoint ending is invalid")),
                },
            },
            _ => return Err(SourceError("unsupported checkpoint event")),
        };
        require(data.0.is_empty(), "checkpoint record has trailing data")?;
        Ok(event)
    }
}

impl Entry {
    fn encode(&self, data: &mut Vec<u8>) -> Result<(), SourceError> {
        require(
            self.width <= EDGE && self.height <= EDGE,
            "checkpoint frame is too large",
        )?;
        data.push(u8::from(self.reset));
        for value in [self.width, self.height, self.tiles.len()] {
            put(data, value)?;
        }
        data.extend_from_slice(self.signature.bytes());
        let geometry: Vec<_> = tiles(self.width, self.height).collect();
        for (index, bytes) in &self.tiles {
            let tile = geometry
                .get(*index)
                .ok_or(SourceError("checkpoint tile is outside the image"))?;
            require(
                bytes.len() <= tile.encoded_limit(),
                "checkpoint tile is too large",
            )?;
            put(data, *index)?;
            put(data, bytes.len())?;
            data.extend_from_slice(bytes);
        }
        Ok(())
    }

    fn decode(data: &mut Cursor<'_>) -> Result<Self, SourceError> {
        let reset = boolean(data)?;
        let (width, height, count) = (data.usize()?, data.usize()?, data.usize()?);
        require(
            (1..=EDGE).contains(&width) && (1..=EDGE).contains(&height),
            "checkpoint frame dimensions are invalid",
        )?;
        let signature = Signature::from_bytes(data.data(9_216)?)
            .ok_or(SourceError("checkpoint signature is truncated"))?;
        let geometry: Vec<_> = tiles(width, height).collect();
        require(
            count <= geometry.len()
                && count <= data.0.len() / 8
                && (!reset || count == geometry.len()),
            "checkpoint tile count is invalid",
        )?;
        let mut versions = Vec::with_capacity(count);
        for position in 0..count {
            let (index, length) = (data.usize()?, data.usize()?);
            let tile = geometry
                .get(index)
                .ok_or(SourceError("checkpoint tile is outside the image"))?;
            require(
                if reset {
                    index == position
                } else {
                    versions
                        .last()
                        .is_none_or(|(previous, _)| *previous < index)
                },
                "checkpoint tiles are out of order",
            )?;
            require(
                length <= tile.encoded_limit(),
                "checkpoint tile is too large",
            )?;
            versions.push((index, data.data(length)?.into()));
        }
        Ok(Self {
            width,
            height,
            reset,
            signature,
            tiles: versions,
        })
    }
}

fn put(bytes: &mut Vec<u8>, value: usize) -> Result<(), SourceError> {
    let value = u32::try_from(value)
        .map_err(|_out_of_range| SourceError("checkpoint count is too large"))?;
    bytes.extend_from_slice(&value.to_le_bytes());
    Ok(())
}
fn boolean(data: &mut Cursor<'_>) -> Result<bool, SourceError> {
    match data.take::<1>()? {
        [0] => Ok(false),
        [1] => Ok(true),
        _ => Err(SourceError("checkpoint flag is invalid")),
    }
}
fn valid_time(at: Moment) -> Result<(), SourceError> {
    require(
        at.as_millis() <= DURATION_MS,
        "checkpoint time is out of range",
    )
}
fn valid_surface(size: Size) -> Result<(), SourceError> {
    require(
        size.width.is_finite()
            && size.height.is_finite()
            && size.width > 0.0
            && size.height > 0.0
            && size.width <= 65_536.0
            && size.height <= 65_536.0,
        "checkpoint surface is invalid",
    )
}

#[cfg(test)]
mod tests;
