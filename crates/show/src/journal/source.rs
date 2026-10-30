//! Bounded, indexed source frames. Callers supply byte ranges and consume encoded chunks; this
//! module owns the format and reconstructs one image without loading the other images.

use std::{fmt, ops::Range};

use super::{
    Journal, PIXEL, Version,
    record::{Cursor, emit_record, payload, require},
    tiles,
};
use crate::{Image, Moment};

const MAGIC: &[u8; 8] = b"SPKSRC01";
const FOOTER: &[u8; 8] = b"SPKIDX01";
const TILE_RECORD: u8 = 1;
const INDEX_RECORD: u8 = 2;
const RECORD_OVERHEAD: usize = 9;
const FOOTER_BYTES: u64 = 16;
const INDEX_LIMIT: usize = 64 << 20;
const FRAME_LIMIT: usize = 4_096;
const EDGE_LIMIT: u32 = super::MAX_FRAME_EDGE;
// The format allows teardown slack beyond the app's five-minute capture limit.
const DURATION_LIMIT_MS: u64 = 360_000;

/// A content-free failure to read or encode source evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceError(pub(crate) &'static str);

impl fmt::Display for SourceError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.0)
    }
}

impl std::error::Error for SourceError {}

/// An index of held source samples. It contains no pixels, narration, or open files.
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

/// The actual sample served for a requested moment, which can fall between retained samples.
#[derive(Debug)]
pub struct SourceFrame {
    /// When this frame was sampled; never the requested time unless it matches.
    pub at: Moment,
    /// The unannotated recorded image.
    pub image: Image,
}

impl Source {
    /// The largest source file accepted, including its index.
    pub const FILE_LIMIT: u64 = 512 << 20;

    /// Reads only the header, footer, and index. `read` must return exactly the requested bytes.
    /// The index's checksum, counts, ordering, geometry, and ranges are checked before use.
    ///
    /// # Errors
    /// Fails for an unsupported or damaged file, a size above the fixed format limits, or a
    /// caller's read error. Format errors never contain source bytes.
    pub fn read<E: From<SourceError>>(
        length: u64,
        mut read: impl FnMut(Range<u64>) -> Result<Vec<u8>, E>,
    ) -> Result<Self, E> {
        require(
            (33..=Self::FILE_LIMIT).contains(&length),
            "source file size is invalid",
        )?;
        require(read(0..8)?.as_slice() == MAGIC, "unsupported source format")?;
        let footer = read(length.saturating_sub(FOOTER_BYTES)..length)?;
        let mut footer = Cursor(&footer);
        let size = footer.wide()?;
        require(footer.0 == FOOTER, "source index footer is invalid")?;
        require(
            size <= u64::try_from(INDEX_LIMIT.saturating_add(RECORD_OVERHEAD)).unwrap_or(u64::MAX),
            "source index is too large",
        )?;
        let end = length.saturating_sub(FOOTER_BYTES);
        let start = end
            .checked_sub(size)
            .filter(|start| *start >= 8)
            .ok_or(SourceError("source index range is invalid"))?;
        let encoded = read(start..end)?;
        require(
            u64::try_from(encoded.len()).ok() == Some(size),
            "source index is truncated",
        )?;
        let mut index = Cursor(payload(&encoded, INDEX_RECORD)?);
        let index_checksum = encoded
            .get(encoded.len().saturating_sub(4)..)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(SourceError("source checksum is missing"))?;
        let duration = Moment::from_millis(index.wide()?);
        require(
            duration.as_millis() <= DURATION_LIMIT_MS,
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
            count > 0 && u64::try_from(count).unwrap_or(u64::MAX) <= arrivals,
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
        let mut expected = 8;
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
            let geometry: Vec<_> =
                tiles(crate::convert::count(width), crate::convert::count(height)).collect();
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

    /// The end of the recorded timeline, which may follow the last sample.
    #[must_use]
    pub const fn duration(&self) -> Moment {
        self.duration
    }

    /// Moments actually retained, strictly increasing. Gaps are missing evidence, not video.
    #[must_use]
    pub fn moments(&self) -> &[Moment] {
        &self.moments
    }

    /// Frames delivered to the journal, including arrivals its memory budget declined.
    #[must_use]
    pub const fn arrivals(&self) -> u64 {
        self.arrivals
    }

    /// The final arrival stride after thinning, starting at one.
    #[must_use]
    pub const fn stride(&self) -> u64 {
        self.stride
    }

    /// The encoded file size, including checksums and the index.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Reconstructs the latest held frame at or before `at`, or the first if `at` precedes it.
    /// Only that frame's compressed tiles are read, each checked before decompression.
    ///
    /// # Errors
    /// Fails for a time beyond the recording, a damaged tile, or a caller's read error. No partial
    /// image is returned, and no decoder message can expose private bytes.
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
        let width = crate::convert::count(segment.width);
        let height = crate::convert::count(segment.height);
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
                u64::try_from(encoded.len()).ok()
                    == Some(location.record.end.saturating_sub(location.record.start)),
                "source tile is truncated",
            )?;
            let version = Version {
                first: 0,
                data: payload(&encoded, TILE_RECORD)?.into(),
            };
            version
                .restore(tile, stride, &mut rgba)
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
    /// Streams the retained journal as source evidence, without building another copy of its
    /// compressed frames. `emit` receives bounded chunks; only the index is assembled in memory.
    /// The caller supplies the recording's end because the last frame may precede it.
    ///
    /// # Errors
    /// Fails when the journal exceeds the source format's limits or `emit` fails. The caller must
    /// discard a partial output. No source file is complete until its footer has been emitted.
    pub fn write_source<E: From<SourceError>>(
        &self,
        duration: Moment,
        mut emit: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        require(
            !self.samples.is_empty()
                && self.samples.len() <= FRAME_LIMIT
                && duration.as_millis() <= DURATION_LIMIT_MS
                && self
                    .samples
                    .last()
                    .is_some_and(|sample| sample.at <= duration),
            "journal exceeds the source format limits",
        )?;
        let mut index = Vec::new();
        index.extend_from_slice(&duration.as_millis().to_le_bytes());
        index.extend_from_slice(
            &u64::try_from(self.arrived)
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        index.extend_from_slice(&u64::try_from(self.stride).unwrap_or(u64::MAX).to_le_bytes());
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
                        u64::try_from(offset).unwrap_or(u64::MAX) < Source::FILE_LIMIT,
                        "source file is too large",
                    )?;
                    emit_record(TILE_RECORD, &version.data, &mut emit)?;
                }
            }
        }
        let index_size = index.len().saturating_add(RECORD_OVERHEAD);
        require(
            u64::try_from(offset.saturating_add(index_size)).unwrap_or(u64::MAX)
                <= Source::FILE_LIMIT.saturating_sub(FOOTER_BYTES),
            "source file is too large",
        )?;
        emit_record(INDEX_RECORD, &index, &mut emit)?;
        emit(&u64::try_from(index_size).unwrap_or(u64::MAX).to_le_bytes())?;
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
        tile: super::Tile,
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
            let limit = tile.encoded_limit();
            require(
                offset == *expected
                    && size >= RECORD_OVERHEAD as u64
                    && size <= u64::try_from(limit).unwrap_or(u64::MAX)
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
