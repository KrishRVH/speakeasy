use super::{
    DURATION_LIMIT, Entry, Journal, SourceError,
    record::{Cursor, RECORD_OVERHEAD, framed, payload, require},
    tiles,
};
use crate::{Ending, Image, Moment, Point, Recording, Rect, Signature, Size, Span, Tracker};

const MAGIC: &[u8; 8] = b"SPKLOG02";
const EDGE: usize = super::MAX_FRAME_EDGE as usize;
const RECORD_LIMIT: usize = 40 << 20;
const EVENT_OVERHEAD: usize = 64;

const HEADER: u8 = 0;
const SURFACE: u8 = 1;
const PIN: u8 = 3;
const ARRIVAL: u8 = 4;
const END: u8 = 5;
const POINTER_GAP: u8 = 6;
const POINTER: u8 = 7;

#[derive(Clone, Debug)]
pub enum TakeEvent {
    Surface(Size),
    Pointer {
        at: Moment,
        point: Point,
        part: Rect,
        drawing: bool,
        pressed: bool,
    },
    PointerGap(Moment),
    Pin(Span),
    Arrival {
        at: Moment,
        entry: Option<Entry>,
    },
    End {
        at: Moment,
        ending: Ending,
    },
}

impl TakeEvent {
    #[must_use]
    pub fn bytes(&self) -> usize {
        match self {
            Self::Arrival {
                entry: Some(entry), ..
            } => entry.bytes().saturating_add(EVENT_OVERHEAD),
            _ => EVENT_OVERHEAD,
        }
    }
}

#[derive(Debug)]
pub struct Take {
    journal: Journal,
    tracker: Tracker,
    latest: Moment,
    ending: Option<Ending>,
}

impl Take {
    #[must_use]
    pub fn new(budget: usize, surface: Size) -> Self {
        Self {
            journal: Journal::with_budget(budget),
            tracker: Tracker::new(surface),
            latest: Moment::START,
            ending: None,
        }
    }

    #[must_use]
    pub const fn journal(&self) -> &Journal {
        &self.journal
    }

    #[must_use]
    pub const fn tracker(&self) -> &Tracker {
        &self.tracker
    }

    #[must_use]
    pub const fn ended(&self) -> bool {
        self.ending.is_some()
    }

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

    pub fn end(&mut self, at: Moment, ending: Ending) -> TakeEvent {
        self.latest = self.latest.max(at.min(DURATION_LIMIT));
        self.ending = Some(ending);
        TakeEvent::End {
            at: self.latest,
            ending,
        }
    }

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
                part,
                drawing,
                pressed,
            } => {
                valid_time(*at)?;
                require(point.is_finite(), "checkpoint pointer is not finite")?;
                require(
                    self.tracker.locate(*part),
                    "checkpoint pointer geometry is invalid",
                )?;
                self.latest = self.latest.max(*at);
                self.tracker.observe(*at, *point, *drawing, *pressed);
            },
            TakeEvent::PointerGap(at) => {
                valid_time(*at)?;
                self.latest = self.latest.max(*at);
                self.tracker.gap(*at);
            },
            TakeEvent::Pin(span) => {
                valid_time(span.end())?;
                self.latest = self.latest.max(span.end());
                self.tracker.pin(*span);
            },
            TakeEvent::Arrival { at, entry } => {
                valid_time(*at)?;
                self.journal.restore(*at, entry.as_ref())?;
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

#[derive(Debug)]
pub struct Log;

impl Log {
    pub const HEADER_BYTES: usize =
        MAGIC.len() + RECORD_OVERHEAD + size_of::<u64>() + 2 * size_of::<f32>();
    pub const MAX_BUDGET: usize = 384 << 20;

    pub fn header(budget: usize, surface: Size) -> Result<Vec<u8>, SourceError> {
        require(
            budget <= Self::MAX_BUDGET,
            "checkpoint memory budget is too large",
        )?;
        valid_surface(surface)?;
        let mut data = Vec::new();
        data.extend_from_slice(&(budget as u64).to_le_bytes());
        data.extend_from_slice(&surface.width.to_le_bytes());
        data.extend_from_slice(&surface.height.to_le_bytes());
        Ok([MAGIC.as_slice(), &framed(HEADER, &data)?].concat())
    }

    pub fn start(header: &[u8]) -> Result<Take, SourceError> {
        require(
            header.len() == Self::HEADER_BYTES && header.get(..MAGIC.len()) == Some(MAGIC),
            "unsupported or truncated checkpoint header",
        )?;
        let mut data = Cursor(payload(
            header.get(MAGIC.len()..).unwrap_or_default(),
            HEADER,
        )?);
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

    pub fn record_size(header: &[u8]) -> Result<usize, SourceError> {
        let length = Cursor(header)
            .usize()?
            .checked_add(RECORD_OVERHEAD)
            .ok_or(SourceError("checkpoint record is too large"))?;
        require(length <= RECORD_LIMIT, "checkpoint record is too large")?;
        Ok(length)
    }

    pub fn encode(event: &TakeEvent) -> Result<Vec<u8>, SourceError> {
        let mut data = Vec::new();
        let kind = match event {
            TakeEvent::Surface(size) => {
                data.extend_from_slice(&size.width.to_le_bytes());
                data.extend_from_slice(&size.height.to_le_bytes());
                SURFACE
            },
            TakeEvent::Pointer {
                at,
                point,
                part,
                drawing,
                pressed,
            } => {
                data.extend_from_slice(&at.as_millis().to_le_bytes());
                data.extend_from_slice(&point.x.to_le_bytes());
                data.extend_from_slice(&point.y.to_le_bytes());
                data.push(u8::from(*drawing));
                data.push(u8::from(*pressed));
                data.extend(
                    [part.left, part.top, part.right, part.bottom]
                        .into_iter()
                        .flat_map(f32::to_le_bytes),
                );
                POINTER
            },
            TakeEvent::Pin(span) => {
                data.extend_from_slice(&span.start().as_millis().to_le_bytes());
                data.extend_from_slice(&span.end().as_millis().to_le_bytes());
                PIN
            },
            TakeEvent::Arrival { at, entry } => {
                data.extend_from_slice(&at.as_millis().to_le_bytes());
                data.push(u8::from(entry.is_some()));
                if let Some(entry) = entry {
                    entry.encode(&mut data)?;
                }
                ARRIVAL
            },
            TakeEvent::End { at, ending } => {
                data.extend_from_slice(&at.as_millis().to_le_bytes());
                data.push(match ending {
                    Ending::Finished => 0,
                    Ending::ScreenStopped => 1,
                    Ending::WindowClosed => 2,
                    Ending::WindowOffScreen => 3,
                });
                END
            },
            TakeEvent::PointerGap(at) => {
                data.extend_from_slice(&at.as_millis().to_le_bytes());
                POINTER_GAP
            },
        };
        require(
            data.len().saturating_add(RECORD_OVERHEAD) <= RECORD_LIMIT,
            "checkpoint record is too large",
        )?;
        framed(kind, &data)
    }

    pub fn decode(record: &[u8]) -> Result<TakeEvent, SourceError> {
        require(
            record.len() <= RECORD_LIMIT,
            "checkpoint record is too large",
        )?;
        let kind = *record
            .get(size_of::<u32>())
            .ok_or(SourceError("checkpoint record is truncated"))?;
        let mut data = Cursor(payload(record, kind)?);
        let event = match kind {
            SURFACE => TakeEvent::Surface(Size {
                width: f32::from_le_bytes(data.take()?),
                height: f32::from_le_bytes(data.take()?),
            }),
            POINTER => TakeEvent::Pointer {
                at: Moment::from_millis(data.wide()?),
                point: Point::new(
                    f32::from_le_bytes(data.take()?),
                    f32::from_le_bytes(data.take()?),
                ),
                drawing: boolean(&mut data)?,
                pressed: boolean(&mut data)?,
                part: Rect::from([
                    f32::from_le_bytes(data.take()?),
                    f32::from_le_bytes(data.take()?),
                    f32::from_le_bytes(data.take()?),
                    f32::from_le_bytes(data.take()?),
                ]),
            },
            PIN => TakeEvent::Pin(Span::new(
                Moment::from_millis(data.wide()?),
                Moment::from_millis(data.wide()?),
            )),
            ARRIVAL => TakeEvent::Arrival {
                at: Moment::from_millis(data.wide()?),
                entry: if boolean(&mut data)? {
                    Some(Entry::decode(&mut data)?)
                } else {
                    None
                },
            },
            END => TakeEvent::End {
                at: Moment::from_millis(data.wide()?),
                ending: match data.take::<1>()? {
                    [0] => Ending::Finished,
                    [1] => Ending::ScreenStopped,
                    [2] => Ending::WindowClosed,
                    [3] => Ending::WindowOffScreen,
                    _ => return Err(SourceError("checkpoint ending is invalid")),
                },
            },
            POINTER_GAP => TakeEvent::PointerGap(Moment::from_millis(data.wide()?)),
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
        let signature = Signature::from_bytes(data.data(Signature::BYTES)?)
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
    require(at <= DURATION_LIMIT, "checkpoint time is out of range")
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
