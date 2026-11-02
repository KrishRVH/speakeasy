use std::{fmt, iter, mem, ops::Range, sync::Arc, time::Duration};

use crate::{
    Image, Moment, convert,
    signature::{Sample, Signature},
};

mod log;
mod record;
mod source;
pub use log::{Log, Take, TakeEvent};
use record::require;
pub use source::{Source, SourceError, SourceFrame};

const TILE: usize = 64;

pub const MAX_FRAME_EDGE: u32 = 2_560;
const PIXEL: usize = 4;
const DURATION_LIMIT: Moment = Moment::from_millis(360_000);

pub struct Journal {
    samples: Vec<Sample>,
    segments: Vec<Segment>,
    bytes: usize,
    budget: usize,
    stride: usize,
    arrived: usize,
    latest: Option<Image>,
}

struct Segment {
    first: usize,
    width: usize,
    height: usize,
    tiles: Vec<Vec<Version>>,
}

struct Version {
    first: usize,
    data: Arc<[u8]>,
}

#[derive(Clone, Copy)]
struct Tile {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

impl Tile {
    fn encoded_limit(self) -> usize {
        self.width
            .saturating_mul(self.height)
            .saturating_mul(PIXEL)
            .saturating_mul(3)
            .div_ceil(2)
            .saturating_add(128)
    }

    fn columns(self) -> Range<usize> {
        self.left.saturating_mul(PIXEL)..self.left.saturating_add(self.width).saturating_mul(PIXEL)
    }
}

impl Default for Journal {
    fn default() -> Self {
        Self::with_budget(usize::MAX)
    }
}

impl fmt::Debug for Journal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Journal")
            .field("frames", &self.samples.len())
            .field("bytes", &self.bytes)
            .field("budget", &self.budget)
            .field("stride", &self.stride)
            .finish_non_exhaustive()
    }
}

impl Journal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub(crate) const fn with_budget(budget: usize) -> Self {
        Self {
            samples: Vec::new(),
            segments: Vec::new(),
            bytes: 0,
            budget,
            stride: 1,
            arrived: 0,
            latest: None,
        }
    }

    pub fn admit(&mut self) -> Option<Admission<'_>> {
        let admitted = self.arrived.is_multiple_of(self.stride);
        self.arrived = self.arrived.saturating_add(1);
        admitted.then_some(Admission(self))
    }

    #[must_use]
    pub fn sealed(mut self) -> Self {
        self.latest = None;
        self
    }

    #[must_use]
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    #[must_use]
    pub fn frame_at(&self, at: Moment) -> Option<Image> {
        let index = self
            .samples
            .partition_point(|sample| sample.at <= at)
            .saturating_sub(1);
        let position = self
            .segments
            .partition_point(|segment| segment.first <= index)
            .saturating_sub(1);
        self.segments.get(position)?.frame(index)
    }

    fn thin(&mut self) {
        let count = self.samples.len();
        let renumbered: Vec<Option<usize>> = (0..count)
            .map(|index| {
                let kept = index.is_multiple_of(2) || index.saturating_add(1) == count;
                kept.then_some(index.div_ceil(2))
            })
            .collect();
        let segments = mem::take(&mut self.segments);
        self.segments = with_ends(segments, |segment| segment.first, count)
            .filter_map(|(segment, end)| segment.thinned(&renumbered, end))
            .collect();
        self.samples = mem::take(&mut self.samples)
            .into_iter()
            .zip(&renumbered)
            .filter_map(|(sample, kept)| kept.map(|_| sample))
            .collect();
        self.bytes = self.segments.iter().map(Segment::bytes).sum();
        self.stride = self.stride.saturating_mul(2);
    }
}

#[derive(Debug)]
#[must_use = "an admitted frame is recorded only through its admission"]
pub struct Admission<'a>(&'a mut Journal);

impl Admission<'_> {
    #[expect(
        clippy::must_use_candidate,
        reason = "Recording mutates the admitted journal; only checkpoint callers need the returned entry"
    )]
    pub fn record(self, at: Moment, frame: Image) -> Entry {
        let Self(journal) = self;
        let (width, height) = (
            convert::count(frame.width()),
            convert::count(frame.height()),
        );
        let before = journal
            .latest
            .as_ref()
            .filter(|before| before.width() == frame.width() && before.height() == frame.height());
        let entry = Entry {
            width,
            height,
            reset: before.is_none(),
            signature: Signature::of(&frame),
            tiles: tiles(width, height)
                .enumerate()
                .filter(|(_, tile)| before.is_none_or(|before| changed(before, &frame, *tile)))
                .map(|(index, tile)| (index, encode_tile(&frame, tile)))
                .collect(),
        };
        journal.push(at, &entry);
        journal.latest = Some(frame);
        entry
    }
}

#[derive(Clone)]
pub struct Entry {
    width: usize,
    height: usize,
    reset: bool,
    signature: Signature,
    tiles: Vec<(usize, Arc<[u8]>)>,
}

impl fmt::Debug for Entry {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Entry")
            .field("size", &(self.width, self.height))
            .field("changed_tiles", &self.tiles.len())
            .finish_non_exhaustive()
    }
}

impl Entry {
    #[must_use]
    pub(crate) fn bytes(&self) -> usize {
        self.tiles.iter().fold(
            9_280_usize.saturating_add(self.tiles.len().saturating_mul(32)),
            |bytes, (_, tile)| bytes.saturating_add(tile.len()),
        )
    }
}

impl Journal {
    fn restore(&mut self, at: Moment, entry: Option<&Entry>) -> Result<(), SourceError> {
        if let Some(entry) = entry {
            require(
                self.arrived.is_multiple_of(self.stride),
                "checkpoint frame disagrees with sampling",
            )?;
            require(
                entry.reset
                    || self.segments.last().is_some_and(|segment| {
                        segment.width == entry.width && segment.height == entry.height
                    }),
                "checkpoint delta has no preceding frame",
            )?;
        }
        if self.admit().is_some()
            && let Some(entry) = entry
        {
            self.push(at, entry);
        }
        Ok(())
    }

    fn push(&mut self, at: Moment, entry: &Entry) {
        let at = self.samples.last().map_or(at, |last| {
            at.max(last.at.saturating_add(Duration::from_millis(1)))
        });
        let index = self.samples.len();
        self.samples.push(Sample {
            at,
            signature: entry.signature.clone(),
        });
        if entry.reset {
            self.segments.push(Segment {
                first: index,
                width: entry.width,
                height: entry.height,
                tiles: entry
                    .tiles
                    .iter()
                    .map(|(_, data)| {
                        vec![Version {
                            first: index,
                            data: Arc::clone(data),
                        }]
                    })
                    .collect(),
            });
        } else if let Some(segment) = self.segments.last_mut() {
            for (tile, data) in &entry.tiles {
                if let Some(versions) = segment.tiles.get_mut(*tile) {
                    versions.push(Version {
                        first: index,
                        data: Arc::clone(data),
                    });
                }
            }
        }
        self.bytes = entry.tiles.iter().fold(self.bytes, |bytes, (_, tile)| {
            bytes.saturating_add(tile.len())
        });
        while self.bytes > self.budget && self.samples.len() > 2 {
            self.thin();
        }
    }
}

impl Segment {
    fn bytes(&self) -> usize {
        self.tiles
            .iter()
            .flatten()
            .map(|version| version.data.len())
            .sum()
    }

    fn thinned(self, renumbered: &[Option<usize>], end: usize) -> Option<Self> {
        let kept_from = |from: usize, until: usize| {
            renumbered
                .get(from..until)?
                .iter()
                .find_map(|number| *number)
        };
        let first = kept_from(self.first, end)?;
        let tiles = self
            .tiles
            .into_iter()
            .map(|versions| {
                with_ends(versions, |version| version.first, end)
                    .filter_map(|(version, until)| {
                        kept_from(version.first, until).map(|first| Version { first, ..version })
                    })
                    .collect()
            })
            .collect();
        Some(Self {
            first,
            tiles,
            ..self
        })
    }

    fn frame(&self, index: usize) -> Option<Image> {
        let stride = self.width.saturating_mul(PIXEL);
        let mut rgba = vec![0; stride.saturating_mul(self.height)];
        for (tile, versions) in tiles(self.width, self.height).zip(&self.tiles) {
            let shown = versions
                .partition_point(|version| version.first <= index)
                .saturating_sub(1);
            restore_tile(&versions.get(shown)?.data, tile, stride, &mut rgba)?;
        }
        Image::from_premultiplied(
            u32::try_from(self.width).ok()?,
            u32::try_from(self.height).ok()?,
            rgba,
        )
        .ok()
    }
}

fn encode_tile(frame: &Image, tile: Tile) -> Arc<[u8]> {
    let mut filtered =
        Vec::with_capacity(tile.width.saturating_mul(tile.height).saturating_mul(PIXEL));
    for row in tile_rows(frame, tile) {
        let mut left = [0_u8; PIXEL];
        for pixel in row.as_chunks::<PIXEL>().0 {
            filtered.extend(
                pixel
                    .iter()
                    .zip(left)
                    .map(|(byte, before)| byte.wrapping_sub(before)),
            );
            left = *pixel;
        }
    }
    fdeflate::compress_to_vec(&filtered).into()
}

fn restore_tile(data: &[u8], tile: Tile, stride: usize, rgba: &mut [u8]) -> Option<()> {
    let columns = tile.columns();
    let row_bytes = columns.len();
    let filtered =
        fdeflate::decompress_to_vec_bounded(data, row_bytes.saturating_mul(tile.height)).ok()?;
    if filtered.len() != row_bytes.checked_mul(tile.height)? {
        return None;
    }
    let rows = rgba.chunks_exact_mut(stride.max(1)).skip(tile.top);
    for (row, filtered_row) in rows.zip(filtered.chunks_exact(row_bytes.max(1))) {
        let mut left = [0_u8; PIXEL];
        for (pixel, deltas) in row
            .get_mut(columns.clone())?
            .as_chunks_mut::<PIXEL>()
            .0
            .iter_mut()
            .zip(filtered_row.as_chunks::<PIXEL>().0)
        {
            for ((byte, delta), before) in pixel.iter_mut().zip(deltas).zip(left) {
                *byte = delta.wrapping_add(before);
            }
            left = *pixel;
        }
    }
    Some(())
}

fn with_ends<T>(
    runs: Vec<T>,
    start: impl Fn(&T) -> usize,
    end: usize,
) -> impl Iterator<Item = (T, usize)> {
    let mut runs = runs.into_iter().peekable();
    iter::from_fn(move || {
        let run = runs.next()?;
        Some((run, runs.peek().map_or(end, &start)))
    })
}

fn tiles(width: usize, height: usize) -> impl Iterator<Item = Tile> {
    (0..height).step_by(TILE).flat_map(move |top| {
        (0..width).step_by(TILE).map(move |left| Tile {
            left,
            top,
            width: TILE.min(width.saturating_sub(left)),
            height: TILE.min(height.saturating_sub(top)),
        })
    })
}

fn tile_rows(image: &Image, tile: Tile) -> impl Iterator<Item = &[u8]> {
    let columns = tile.columns();
    image
        .rows()
        .skip(tile.top)
        .take(tile.height)
        .filter_map(move |row| row.get(columns.clone()))
}

fn changed(before: &Image, after: &Image, tile: Tile) -> bool {
    tile_rows(before, tile).ne(tile_rows(after, tile))
}

#[cfg(test)]
mod tests;
