//! The frames a Show samples, kept losslessly beside their signatures within a memory budget.
//!
//! Frames are cut into tiles, and each frame stores only the tiles that changed since the frame
//! before it. A still screen therefore costs almost nothing, and every frame held rebuilds exactly.
//! A journal over its budget thins evenly, and each frame's [`Sample`] is dropped with its tiles,
//! so selection only picks frames the journal holds.

use std::{fmt, iter, mem, ops::Range, time::Duration};

use crate::{
    Image, Moment, convert,
    signature::{Sample, Signature},
};

/// Tile edge in pixels: small enough that a blinking caret stores little, large enough that the
/// index stays small.
const TILE: usize = 64;
const PIXEL: usize = 4;

/// Recorded frames and their signatures, in time order.
pub struct Journal {
    /// One per frame held.
    samples: Vec<Sample>,
    /// Runs of frames of one size; a resolution change starts the next.
    segments: Vec<Segment>,
    /// Compressed bytes held.
    bytes: usize,
    /// The most compressed bytes held once more than two frames are.
    budget: usize,
    /// Every `stride`th arrival is admitted; it doubles whenever the journal thins.
    stride: usize,
    /// Arrivals counted by [`Journal::admit`].
    arrived: usize,
    /// The last recorded frame, which the next is compared against; absent before the first frame
    /// and once sealed.
    latest: Option<Image>,
}

struct Segment {
    /// The index in [`Journal::samples`] of the segment's first frame.
    first: usize,
    width: usize,
    height: usize,
    /// Each tile's versions, in recording order; the first belongs to the segment's first frame.
    tiles: Vec<Vec<Version>>,
}

/// A tile as one frame left it: rows of pixels, each byte replaced by its difference from the
/// same byte of the pixel to its left, then deflated.
struct Version {
    /// The index of the first frame that shows this version.
    first: usize,
    data: Box<[u8]>,
}

/// One tile's place in its frame, in pixels.
#[derive(Clone, Copy)]
struct Tile {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

impl Tile {
    /// The bytes the tile covers in each row of its frame.
    fn columns(self) -> Range<usize> {
        self.left.saturating_mul(PIXEL)..self.left.saturating_add(self.width).saturating_mul(PIXEL)
    }
}

impl Default for Journal {
    fn default() -> Self {
        Self::with_budget(usize::MAX)
    }
}

/// Summarizes the journal; its tiles are screen content and never print.
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
    /// An empty journal without a budget.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty journal that holds at most `budget` compressed bytes. Each time it runs past the
    /// budget, every second frame is dropped and arrivals are admitted half as often from then on,
    /// so a long scroll or video keeps even coverage within it. Two frames are always held,
    /// whatever they cost.
    #[must_use]
    pub const fn with_budget(budget: usize) -> Self {
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

    /// Counts a frame's arrival and, when the budget keeps it, returns the admission that records
    /// it, so a frame the budget would drop costs nothing to prepare.
    pub fn admit(&mut self) -> Option<Admission<'_>> {
        let admitted = self.arrived.is_multiple_of(self.stride);
        self.arrived = self.arrived.saturating_add(1);
        admitted.then_some(Admission(self))
    }

    /// The journal without the frame it keeps only to compare the next one against, for when
    /// recording ends. Every frame held still rebuilds exactly; a frame recorded later starts a new
    /// run of frames.
    #[must_use]
    pub fn sealed(mut self) -> Self {
        self.latest = None;
        self
    }

    /// The signature of every frame held, in time order.
    #[must_use]
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    /// Whether no frame is held.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Compressed bytes held.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// The frame on screen at `at`: the latest recorded at or before it, else the first.
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

    /// Drops every second frame and its sample, keeping the first and the last, and admits
    /// arrivals half as often from then on, so a journal over its budget keeps even coverage of
    /// the whole recording. Every kept frame still rebuilds exactly.
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

/// A frame's arrival the journal admitted. Only [`Journal::admit`] makes one, so every frame
/// recorded counts toward how often arrivals are admitted.
#[derive(Debug)]
#[must_use = "an admitted frame is recorded only through its admission"]
pub struct Admission<'a>(&'a mut Journal);

impl Admission<'_> {
    /// Records `frame` and its signature as on screen at `at`, then thins the journal while it is
    /// over budget. A moment no later than the last recorded one moves just past it, so every
    /// frame keeps a moment of its own and [`frame_at`](Journal::frame_at) rebuilds the frame
    /// each sample fingerprints.
    pub fn record(self, at: Moment, frame: Image) {
        let Self(journal) = self;
        let at = journal.samples.last().map_or(at, |last| {
            at.max(last.at.saturating_add(Duration::from_millis(1)))
        });
        let index = journal.samples.len();
        journal.samples.push(Sample {
            at,
            signature: Signature::of(&frame),
        });
        let added = if let (Some(before), Some(segment)) =
            (&journal.latest, journal.segments.last_mut())
            && before.width() == frame.width()
            && before.height() == frame.height()
        {
            segment.record(index, before, &frame)
        } else {
            let segment = Segment::new(index, &frame);
            let bytes = segment.bytes();
            journal.segments.push(segment);
            bytes
        };
        journal.bytes = journal.bytes.saturating_add(added);
        journal.latest = Some(frame);
        while journal.bytes > journal.budget && journal.samples.len() > 2 {
            journal.thin();
        }
    }
}

impl Segment {
    fn new(first: usize, frame: &Image) -> Self {
        let (width, height) = (
            convert::count(frame.width()),
            convert::count(frame.height()),
        );
        let tiles = tiles(width, height)
            .map(|tile| vec![Version::of(first, frame, tile)])
            .collect();
        Self {
            first,
            width,
            height,
            tiles,
        }
    }

    fn bytes(&self) -> usize {
        self.tiles
            .iter()
            .flatten()
            .map(|version| version.data.len())
            .sum()
    }

    /// The segment with only the frames `renumbered` keeps, numbered anew; `end` is the index after
    /// its last frame. A tile version passes to the first kept frame that shows it, so the frames
    /// after a dropped one rebuild unchanged. `None` when no frame of the segment is kept.
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

    /// Stores the tiles of `frame` that differ from `before`, the frame recorded just before it;
    /// returns the bytes added.
    fn record(&mut self, index: usize, before: &Image, frame: &Image) -> usize {
        let mut added = 0_usize;
        for (tile, versions) in tiles(self.width, self.height).zip(&mut self.tiles) {
            if changed(before, frame, tile) {
                let version = Version::of(index, frame, tile);
                added = added.saturating_add(version.data.len());
                versions.push(version);
            }
        }
        added
    }

    fn frame(&self, index: usize) -> Option<Image> {
        let stride = self.width.saturating_mul(PIXEL);
        let mut rgba = vec![0; stride.saturating_mul(self.height)];
        for (tile, versions) in tiles(self.width, self.height).zip(&self.tiles) {
            let shown = versions
                .partition_point(|version| version.first <= index)
                .saturating_sub(1);
            versions.get(shown)?.restore(tile, stride, &mut rgba)?;
        }
        Image::from_premultiplied(
            u32::try_from(self.width).ok()?,
            u32::try_from(self.height).ok()?,
            rgba,
        )
        .ok()
    }
}

impl Version {
    fn of(first: usize, frame: &Image, tile: Tile) -> Self {
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
        Self {
            first,
            data: fdeflate::compress_to_vec(&filtered).into_boxed_slice(),
        }
    }

    /// Writes this version of the tile into `rgba`, a frame `stride` bytes wide.
    fn restore(&self, tile: Tile, stride: usize, rgba: &mut [u8]) -> Option<()> {
        let columns = tile.columns();
        let row_bytes = columns.len();
        let filtered =
            fdeflate::decompress_to_vec_bounded(&self.data, row_bytes.saturating_mul(tile.height))
                .ok()?;
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
}

/// Pairs each run of frames with the index just past it: where the next run starts, or `end`
/// after the last.
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

/// The tiles of a `width × height` frame, row by row.
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

/// The rows of `image` that `tile` covers, cut to its columns.
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
