use std::{fmt, ops::Range};

use super::{
    DURATION_LIMIT, Journal, PIXEL, Tile,
    record::{Cursor, RECORD_OVERHEAD, emit_record, payload, require},
    restore_tile, tiles,
};
use crate::{Image, Moment, Span, convert};

const MAGIC: &[u8; 8] = b"SPKSRC01";
const FOOTER: &[u8; 8] = b"SPKIDX01";
const TILE_RECORD: u8 = 1;
const INDEX_RECORD: u8 = 2;
const FOOTER_BYTES: u64 = 16;
const SMALLEST_FILE: u64 = (MAGIC.len() + RECORD_OVERHEAD) as u64 + FOOTER_BYTES;
const INDEX_LIMIT: usize = 64 << 20;
const FRAME_LIMIT: usize = 4_096;
const EDGE_LIMIT: u32 = super::MAX_FRAME_EDGE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceError(pub(crate) &'static str);

impl fmt::Display for SourceError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.0)
    }
}

impl std::error::Error for SourceError {}

#[derive(Debug)]
pub struct Source {
    duration: Moment,
    moments: Vec<Moment>,
    arrivals: u64,
    stride: u64,
    bytes: u64,
    index_checksum: [u8; 4],
    segments: Vec<Segment>,
}

#[derive(Debug)]
struct Segment {
    first: usize,
    width: u32,
    height: u32,
    tiles: Vec<Vec<Location>>,
}

#[derive(Debug)]
struct Location {
    first: usize,
    record: Range<u64>,
}

#[derive(Debug)]
pub struct SourceFrame {
    pub at: Moment,
    pub image: Image,
}

impl Source {
    pub const FILE_LIMIT: u64 = 512 << 20;

    pub fn read<E: From<SourceError>>(
        length: u64,
        mut read: impl FnMut(Range<u64>) -> Result<Vec<u8>, E>,
    ) -> Result<Self, E> {
        require(
            (SMALLEST_FILE..=Self::FILE_LIMIT).contains(&length),
            "source file size is invalid",
        )?;
        require(
            read(0..MAGIC.len() as u64)?.as_slice() == MAGIC,
            "unsupported source format",
        )?;
        let footer = read(length.saturating_sub(FOOTER_BYTES)..length)?;
        let mut footer = Cursor(&footer);
        let size = footer.wide()?;
        require(footer.0 == FOOTER, "source index footer is invalid")?;
        require(
            size <= (INDEX_LIMIT + RECORD_OVERHEAD) as u64,
            "source index is too large",
        )?;
        let end = length.saturating_sub(FOOTER_BYTES);
        let start = end
            .checked_sub(size)
            .filter(|start| *start >= MAGIC.len() as u64)
            .ok_or(SourceError("source index range is invalid"))?;
        let encoded = read(start..end)?;
        require(encoded.len() as u64 == size, "source index is truncated")?;
        let mut index = Cursor(payload(&encoded, INDEX_RECORD)?);
        let index_checksum = encoded
            .get(encoded.len().saturating_sub(4)..)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(SourceError("source checksum is missing"))?;
        let duration = Moment::from_millis(index.wide()?);
        require(
            duration <= DURATION_LIMIT,
            "source duration exceeds the format limit",
        )?;
        let arrivals = index.wide()?;
        let stride = index.wide()?;
        require(
            stride.is_power_of_two(),
            "source sampling stride is invalid",
        )?;
        let count = index.count(FRAME_LIMIT, 8)?;
        require(
            count > 0 && count as u64 <= arrivals,
            "source sample count is invalid",
        )?;
        let mut moments = Vec::with_capacity(count);
        for _ in 0..count {
            let at = Moment::from_millis(index.wide()?);
            require(
                at <= duration && moments.last().is_none_or(|previous| *previous < at),
                "source sample times are invalid",
            )?;
            moments.push(at);
        }
        let count = index.count(moments.len(), 16)?;
        require(count > 0, "source has no frame geometry")?;
        let mut segments = Vec::with_capacity(count);
        let mut expected = MAGIC.len() as u64;
        for position in 0..count {
            let first = index.usize()?;
            require(
                first < moments.len()
                    && if position == 0 {
                        first == 0
                    } else {
                        segments
                            .last()
                            .is_some_and(|segment: &Segment| segment.first < first)
                    },
                "source geometry order is invalid",
            )?;
            let width = index.word()?;
            let height = index.word()?;
            require(
                (1..=EDGE_LIMIT).contains(&width) && (1..=EDGE_LIMIT).contains(&height),
                "source frame dimensions exceed the format limit",
            )?;
            let geometry: Vec<_> = tiles(convert::count(width), convert::count(height)).collect();
            let count = index.count(geometry.len(), 4)?;
            require(count == geometry.len(), "source tile count is invalid")?;
            let locations = geometry
                .into_iter()
                .map(|tile| index.versions(first..moments.len(), tile, &mut expected, start))
                .collect::<Result<_, _>>()?;
            segments.push(Segment {
                first,
                width,
                height,
                tiles: locations,
            });
        }
        require(
            index.0.is_empty() && expected == start,
            "source index does not cover its data",
        )?;
        for (position, segment) in segments.iter().enumerate() {
            let end = segments
                .get(position.saturating_add(1))
                .map_or(moments.len(), |next| next.first);
            require(
                segment
                    .tiles
                    .iter()
                    .flatten()
                    .all(|version| version.first < end),
                "source tile outlives its geometry",
            )?;
        }
        Ok(Self {
            duration,
            moments,
            arrivals,
            stride,
            bytes: length,
            index_checksum,
            segments,
        })
    }

    #[must_use]
    pub const fn duration(&self) -> Moment {
        self.duration
    }

    #[must_use]
    pub fn moments(&self) -> &[Moment] {
        &self.moments
    }

    #[must_use]
    pub const fn arrivals(&self) -> u64 {
        self.arrivals
    }

    #[must_use]
    pub const fn stride(&self) -> u64 {
        self.stride
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub fn stable_geometry(&self, window: Span) -> bool {
        let first = self
            .moments
            .partition_point(|at| *at <= window.start())
            .saturating_sub(1);
        let last = self
            .moments
            .partition_point(|at| *at <= window.end())
            .saturating_sub(1);
        let segment = self
            .segments
            .partition_point(|segment| segment.first <= first)
            .saturating_sub(1);
        let mut sizes = self
            .segments
            .iter()
            .skip(segment)
            .take_while(|segment| segment.first <= last)
            .map(|segment| (segment.width, segment.height));
        let first = sizes.next();
        sizes.all(|size| Some(size) == first)
    }

    pub fn frame<E: From<SourceError>>(
        &self,
        at: Moment,
        mut read: impl FnMut(Range<u64>) -> Result<Vec<u8>, E>,
    ) -> Result<SourceFrame, E> {
        require(at <= self.duration, "requested time is beyond the Show")?;
        let end = self.bytes.saturating_sub(FOOTER_BYTES);
        require(
            read(end.saturating_sub(4)..end)?.as_slice() == self.index_checksum,
            "source index changed; inspect the Show again",
        )?;
        let frame = self
            .moments
            .partition_point(|moment| *moment <= at)
            .saturating_sub(1);
        let segment = self
            .segments
            .iter()
            .rev()
            .find(|segment| segment.first <= frame)
            .ok_or(SourceError("source frame geometry is missing"))?;
        let width = convert::count(segment.width);
        let height = convert::count(segment.height);
        let stride = width.saturating_mul(PIXEL);
        let mut rgba = vec![0; stride.saturating_mul(height)];
        for (tile, versions) in tiles(width, height).zip(&segment.tiles) {
            let location = versions
                .iter()
                .rev()
                .find(|version| version.first <= frame)
                .ok_or(SourceError("source tile is missing"))?;
            let encoded = read(location.record.clone())?;
            require(
                encoded.len() as u64 == location.record.end.saturating_sub(location.record.start),
                "source tile is truncated",
            )?;
            restore_tile(payload(&encoded, TILE_RECORD)?, tile, stride, &mut rgba)
                .ok_or(SourceError("source tile is damaged"))?;
        }
        let image = Image::from_premultiplied(segment.width, segment.height, rgba)
            .map_err(|_dimensions| SourceError("source image dimensions are invalid"))?;
        let at = self
            .moments
            .get(frame)
            .copied()
            .ok_or(SourceError("source sample is missing"))?;
        Ok(SourceFrame { at, image })
    }
}

impl Journal {
    pub fn write_source<E: From<SourceError>>(
        &self,
        duration: Moment,
        mut emit: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        require(
            !self.samples.is_empty()
                && self.samples.len() <= FRAME_LIMIT
                && duration <= DURATION_LIMIT
                && self
                    .samples
                    .last()
                    .is_some_and(|sample| sample.at <= duration),
            "journal exceeds the source format limits",
        )?;
        let mut index = Vec::new();
        index.extend_from_slice(&duration.as_millis().to_le_bytes());
        index.extend_from_slice(&(self.arrived as u64).to_le_bytes());
        index.extend_from_slice(&(self.stride as u64).to_le_bytes());
        word(&mut index, self.samples.len())?;
        for sample in &self.samples {
            index.extend_from_slice(&sample.at.as_millis().to_le_bytes());
        }
        word(&mut index, self.segments.len())?;
        emit(MAGIC)?;
        let mut offset: usize = MAGIC.len();
        for segment in &self.segments {
            require(
                segment.width <= EDGE_LIMIT as usize && segment.height <= EDGE_LIMIT as usize,
                "source frame dimensions exceed the format limit",
            )?;
            for value in [
                segment.first,
                segment.width,
                segment.height,
                segment.tiles.len(),
            ] {
                word(&mut index, value)?;
            }
            for (tile, versions) in tiles(segment.width, segment.height).zip(&segment.tiles) {
                word(&mut index, versions.len())?;
                for version in versions {
                    let length = version.data.len().saturating_add(RECORD_OVERHEAD);
                    require(length <= tile.encoded_limit(), "source tile is too large")?;
                    word(&mut index, version.first)?;
                    word(&mut index, offset)?;
                    word(&mut index, length)?;
                    offset = offset
                        .checked_add(length)
                        .ok_or(SourceError("source file is too large"))?;
                    require(
                        (offset as u64) < Source::FILE_LIMIT,
                        "source file is too large",
                    )?;
                    emit_record(TILE_RECORD, &version.data, &mut emit)?;
                }
            }
        }
        let index_size = index.len().saturating_add(RECORD_OVERHEAD);
        require(
            (offset.saturating_add(index_size) as u64)
                <= Source::FILE_LIMIT.saturating_sub(FOOTER_BYTES),
            "source file is too large",
        )?;
        emit_record(INDEX_RECORD, &index, &mut emit)?;
        emit(&(index_size as u64).to_le_bytes())?;
        emit(FOOTER)
    }
}

fn word(index: &mut Vec<u8>, value: usize) -> Result<(), SourceError> {
    require(
        index.len().saturating_add(4) <= INDEX_LIMIT,
        "source index is too large",
    )?;
    let value = u32::try_from(value)
        .map_err(|_out_of_range| SourceError("source index value is too large"))?;
    index.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

impl Cursor<'_> {
    fn versions(
        &mut self,
        frames: Range<usize>,
        tile: Tile,
        expected: &mut u64,
        data_end: u64,
    ) -> Result<Vec<Location>, SourceError> {
        let count = self.count(frames.end.saturating_sub(frames.start), 12)?;
        require(count > 0, "source tile has no initial image")?;
        let mut versions = Vec::with_capacity(count);
        for position in 0..count {
            let first = self.usize()?;
            let ordered = if position == 0 {
                first == frames.start
            } else {
                versions
                    .last()
                    .is_some_and(|version: &Location| version.first < first)
            };
            require(
                first < frames.end && ordered,
                "source tile versions are out of order",
            )?;
            let offset = u64::from(self.word()?);
            let size = u64::from(self.word()?);
            let end = offset
                .checked_add(size)
                .ok_or(SourceError("source tile range overflows"))?;
            require(
                offset == *expected
                    && size >= RECORD_OVERHEAD as u64
                    && size <= tile.encoded_limit() as u64
                    && end <= data_end,
                "source tile range is invalid",
            )?;
            *expected = end;
            versions.push(Location {
                first,
                record: offset..end,
            });
        }
        Ok(versions)
    }
}

#[cfg(test)]
mod tests;
