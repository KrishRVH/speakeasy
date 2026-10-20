//! Every frame a Show samples, kept losslessly in little memory.
//!
//! Frames are cut into tiles, and each frame stores only the tiles that changed since the frame
//! before it. A still screen therefore costs almost nothing, and any recorded moment can be
//! rebuilt exactly, for keyframes now and closer looks later.

use std::mem;

use crate::{Image, Moment, convert};

/// Tile edge in pixels: small enough that a blinking caret stores little, large enough that the
/// index stays small.
const TILE: usize = 64;
const PIXEL: usize = 4;

/// Recorded frames, in time order.
#[derive(Debug, Default)]
pub struct Journal {
    moments: Vec<Moment>,
    /// Runs of frames of one size; a resolution change starts the next.
    segments: Vec<Segment>,
    /// Compressed bytes held.
    bytes: usize,
}

#[derive(Debug)]
struct Segment {
    /// The index in [`Journal::moments`] of the segment's first frame.
    first: usize,
    width: usize,
    height: usize,
    /// Each tile's versions, in recording order; the first belongs to the segment's first frame.
    tiles: Vec<Vec<Version>>,
    /// The latest frame, which the next one is compared against.
    latest: Image,
}

/// A tile as one frame left it: rows of pixels, each byte replaced by its difference from the
/// same byte of the pixel to its left, then deflated.
#[derive(Debug)]
struct Version {
    frame: usize,
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

impl Journal {
    /// An empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `frame` as on screen at `at`, which must not precede the last recorded moment.
    pub fn record(&mut self, at: Moment, frame: Image) {
        let at = self.moments.last().map_or(at, |last| at.max(*last));
        let index = self.moments.len();
        self.moments.push(at);
        let size = (
            convert::count(frame.width()),
            convert::count(frame.height()),
        );
        let continues = self
            .segments
            .last()
            .is_some_and(|segment| (segment.width, segment.height) == size);
        let added = if continues && let Some(segment) = self.segments.last_mut() {
            segment.record(index, frame)
        } else {
            let segment = Segment::new(index, frame);
            let bytes = segment.bytes();
            self.segments.push(segment);
            bytes
        };
        self.bytes = self.bytes.saturating_add(added);
    }

    /// The moments recorded, in time order.
    #[must_use]
    pub fn moments(&self) -> &[Moment] {
        &self.moments
    }

    /// Compressed bytes held, which a recorder can keep within a budget.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Drops every second frame, keeping the first and the last, so a recorder over its budget
    /// keeps even coverage of the whole recording. Every kept frame still rebuilds exactly.
    pub fn thin(&mut self) {
        let count = self.moments.len();
        let mut next = 0_usize;
        let renumbered: Vec<Option<usize>> = (0..count)
            .map(|index| {
                let kept = index.is_multiple_of(2) || index.saturating_add(1) == count;
                kept.then(|| {
                    let number = next;
                    next = next.saturating_add(1);
                    number
                })
            })
            .collect();
        let ends: Vec<usize> = self
            .segments
            .iter()
            .skip(1)
            .map(|segment| segment.first)
            .chain([count])
            .collect();
        self.segments = mem::take(&mut self.segments)
            .into_iter()
            .zip(ends)
            .filter_map(|(segment, end)| segment.thinned(&renumbered, end))
            .collect();
        self.moments = self
            .moments
            .iter()
            .zip(&renumbered)
            .filter_map(|(moment, kept)| kept.map(|_| *moment))
            .collect();
        self.bytes = self.segments.iter().map(Segment::bytes).sum();
    }

    /// The frame on screen at `at`: the latest recorded at or before it, else the first.
    #[must_use]
    pub fn frame_at(&self, at: Moment) -> Option<Image> {
        let index = self
            .moments
            .partition_point(|moment| *moment <= at)
            .saturating_sub(1);
        let position = self
            .segments
            .partition_point(|segment| segment.first <= index)
            .saturating_sub(1);
        self.segments.get(position)?.frame(index)
    }
}

impl Segment {
    fn new(first: usize, frame: Image) -> Self {
        let (width, height) = (
            convert::count(frame.width()),
            convert::count(frame.height()),
        );
        let tiles = tiles(width, height)
            .map(|tile| vec![Version::of(first, &frame, tile)])
            .collect();
        Self {
            first,
            width,
            height,
            tiles,
            latest: frame,
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
                let ends: Vec<usize> = versions
                    .iter()
                    .skip(1)
                    .map(|version| version.frame)
                    .chain([end])
                    .collect();
                versions
                    .into_iter()
                    .zip(ends)
                    .filter_map(|(version, until)| {
                        kept_from(version.frame, until).map(|frame| Version { frame, ..version })
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

    /// Stores the tiles of `frame` that differ from the latest frame; returns the bytes added.
    fn record(&mut self, index: usize, frame: Image) -> usize {
        let mut added = 0_usize;
        for (tile, versions) in tiles(self.width, self.height).zip(&mut self.tiles) {
            if changed(&self.latest, &frame, tile) {
                let version = Version::of(index, &frame, tile);
                added = added.saturating_add(version.data.len());
                versions.push(version);
            }
        }
        self.latest = frame;
        added
    }

    fn frame(&self, index: usize) -> Option<Image> {
        let stride = self.width.saturating_mul(PIXEL);
        let mut rgba = vec![0; stride.saturating_mul(self.height)];
        for (tile, versions) in tiles(self.width, self.height).zip(&self.tiles) {
            let latest = versions.partition_point(|version| version.frame <= index);
            let version = versions.get(latest.saturating_sub(1))?;
            version.restore(tile, stride, &mut rgba)?;
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
    fn of(frame: usize, image: &Image, tile: Tile) -> Self {
        let mut filtered =
            Vec::with_capacity(tile.width.saturating_mul(tile.height).saturating_mul(PIXEL));
        for row in tile_rows(image, tile) {
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
            frame,
            data: fdeflate::compress_to_vec(&filtered).into_boxed_slice(),
        }
    }

    /// Writes this version of the tile into `rgba`, a frame `stride` bytes wide.
    fn restore(&self, tile: Tile, stride: usize, rgba: &mut [u8]) -> Option<()> {
        let row_bytes = tile.width.saturating_mul(PIXEL);
        let size = row_bytes.saturating_mul(tile.height);
        let filtered = fdeflate::decompress_to_vec_bounded(&self.data, size).ok()?;
        for (row, filtered) in filtered.chunks_exact(row_bytes.max(1)).enumerate() {
            let start = tile
                .top
                .saturating_add(row)
                .saturating_mul(stride)
                .saturating_add(tile.left.saturating_mul(PIXEL));
            let target = rgba.get_mut(start..start.saturating_add(row_bytes))?;
            let mut left = [0_u8; PIXEL];
            for (pixel, deltas) in target
                .as_chunks_mut::<PIXEL>()
                .0
                .iter_mut()
                .zip(filtered.as_chunks::<PIXEL>().0)
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
    let columns =
        tile.left.saturating_mul(PIXEL)..tile.left.saturating_add(tile.width).saturating_mul(PIXEL);
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
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn frame(width: u32, height: u32, paint: impl Fn(usize, usize) -> [u8; 4]) -> Image {
        let pixels: Vec<u8> = (0..convert::count(height))
            .flat_map(|y| (0..convert::count(width)).map(move |x| (x, y)))
            .flat_map(|(x, y)| paint(x, y))
            .collect();
        Image::from_rgba(width, height, pixels).unwrap()
    }

    fn at(millis: u64) -> Moment {
        Moment::from_millis(millis)
    }

    #[test]
    fn a_still_screen_costs_almost_nothing_and_changes_cost_their_tiles() {
        let text = |x: usize, y: usize| {
            if (x / 3).wrapping_add(y / 5).is_multiple_of(4) {
                [20, 20, 20, 255]
            } else {
                [240, 240, 240, 255]
            }
        };
        let mut journal = Journal::new();
        journal.record(at(0), frame(640, 360, text));
        let first = journal.bytes();
        for millis in (200..=2_000).step_by(200) {
            journal.record(at(millis), frame(640, 360, text));
        }
        assert_eq!(
            journal.bytes(),
            first,
            "an unchanged screen stores nothing more"
        );
        // A caret appearing changes one tile.
        let caret = |x: usize, y: usize| {
            if (100..102).contains(&x) && (40..60).contains(&y) {
                [0, 0, 0, 255]
            } else {
                text(x, y)
            }
        };
        journal.record(at(2_200), frame(640, 360, caret));
        let added = journal.bytes().saturating_sub(first);
        assert!(added > 0 && added < first / 20, "{added} of {first}");
    }

    #[test]
    fn any_recorded_moment_is_rebuilt_exactly() {
        let gradient = |shift: usize| {
            move |x: usize, y: usize| {
                let value = u8::try_from(x.wrapping_add(y).wrapping_add(shift) % 256).unwrap();
                [value, value / 2, value.wrapping_neg(), 255]
            }
        };
        let mut journal = Journal::new();
        let frames: Vec<Image> = [0, 7, 7, 30]
            .map(|shift| frame(130, 70, gradient(shift)))
            .to_vec();
        for (index, image) in frames.iter().enumerate() {
            journal.record(
                at(u64::try_from(index).unwrap().wrapping_mul(100)),
                image.clone(),
            );
        }
        for (index, image) in frames.iter().enumerate() {
            let moment = at(u64::try_from(index)
                .unwrap()
                .wrapping_mul(100)
                .wrapping_add(50));
            assert_eq!(
                journal.frame_at(moment).as_ref(),
                Some(image),
                "frame {index}"
            );
        }
        assert_eq!(journal.frame_at(at(0)).as_ref(), frames.first());
        // A resolution change starts a new run of frames and leaves the old ones intact.
        let resized = frame(64, 64, gradient(3));
        journal.record(at(1_000), resized.clone());
        assert_eq!(journal.frame_at(at(1_000)), Some(resized));
        assert_eq!(journal.frame_at(at(150)).as_ref(), frames.get(1));
        assert_eq!(Journal::new().frame_at(at(0)), None);
    }

    proptest! {
        #[test]
        fn thinning_keeps_every_other_frame_exact(
            changes in prop::collection::vec((0_u8..4, 0_usize..3), 1..24),
            rounds in 1_usize..4,
        ) {
            // Each step repaints one quadrant, keeps the screen, or switches between two sizes.
            let mut journal = Journal::new();
            let mut recorded = Vec::new();
            let mut shade = [0_u8; 4];
            for (index, (quadrant, size)) in changes.into_iter().enumerate() {
                if let Some(value) = shade.get_mut(usize::from(quadrant)) {
                    *value = value.wrapping_add(40);
                }
                let (width, height) = if size == 2 { (70, 70) } else { (150, 90) };
                let painted = shade;
                let image = frame(width, height, move |x, y| {
                    let quadrant = usize::from(x >= 64).wrapping_add(usize::from(y >= 64).wrapping_mul(2));
                    let value = *painted.get(quadrant).unwrap();
                    [value, value, value, 255]
                });
                let moment = at(u64::try_from(index).unwrap().wrapping_mul(100));
                journal.record(moment, image.clone());
                recorded.push((moment, image));
            }
            for _ in 0..rounds {
                let before = journal.bytes();
                journal.thin();
                let count = recorded.len();
                recorded = recorded
                    .into_iter()
                    .enumerate()
                    .filter(|(index, _)| index.is_multiple_of(2) || index.wrapping_add(1) == count)
                    .map(|(_, frame)| frame)
                    .collect();
                prop_assert!(journal.bytes() <= before);
            }
            let moments: Vec<Moment> = recorded.iter().map(|(moment, _)| *moment).collect();
            prop_assert_eq!(journal.moments(), moments.as_slice());
            for (moment, image) in &recorded {
                prop_assert_eq!(journal.frame_at(*moment), Some(image.clone()));
            }
        }

        #[test]
        fn rebuilding_is_lossless_for_any_size(width in 1_u32..150, height in 1_u32..150, seed in 0_u8..=255) {
            let noise = move |x: usize, y: usize| {
                let mixed = x.wrapping_mul(31).wrapping_add(y.wrapping_mul(17)).wrapping_add(usize::from(seed));
                let value = u8::try_from(mixed % 251).unwrap();
                [value, value.wrapping_mul(3), seed, 255]
            };
            let image = frame(width, height, noise);
            let mut journal = Journal::new();
            journal.record(at(5), image.clone());
            prop_assert_eq!(journal.frame_at(at(5)), Some(image));
        }
    }
}
