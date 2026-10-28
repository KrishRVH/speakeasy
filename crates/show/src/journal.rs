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
                with_ends(versions, |version| version.frame, end)
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

    /// The next arrival at a journal without a budget, which admits every one.
    fn admitted(journal: &mut Journal) -> Admission<'_> {
        journal
            .admit()
            .expect("a journal without a budget admits every arrival")
    }

    /// A frame of four flat quadrants in `shade`, 70 × 70 when `size` is 2 and 150 × 90 otherwise.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "A quadrant index under 4; debug overflow checks fail the test"
    )]
    fn quadrants(shade: [u8; 4], size: usize) -> Image {
        let (width, height) = if size == 2 { (70, 70) } else { (150, 90) };
        frame(width, height, move |x, y| {
            let quadrant = usize::from(x >= 64) + usize::from(y >= 64) * 2;
            let value = *shade.get(quadrant).unwrap();
            [value, value, value, 255]
        })
    }

    #[test]
    fn a_still_screen_costs_almost_nothing_and_changes_cost_their_tiles() {
        let text = |x: usize, y: usize| {
            if (x / 3 + y / 5).is_multiple_of(4) {
                [20, 20, 20, 255]
            } else {
                [240, 240, 240, 255]
            }
        };
        let mut journal = Journal::new();
        admitted(&mut journal).record(at(0), frame(640, 360, text));
        let first = journal.bytes();
        for millis in (200..=2_000).step_by(200) {
            admitted(&mut journal).record(at(millis), frame(640, 360, text));
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
        admitted(&mut journal).record(at(2_200), frame(640, 360, caret));
        let added = journal.bytes() - first;
        assert!(added > 0 && added < first / 20, "{added} of {first}");
    }

    #[test]
    fn any_recorded_moment_is_rebuilt_exactly() {
        let gradient = |shift: usize| {
            move |x: usize, y: usize| {
                let value = u8::try_from((x + y + shift) % 256).unwrap();
                [value, value / 2, value.wrapping_neg(), 255]
            }
        };
        let mut journal = Journal::new();
        let frames: Vec<Image> = [0, 7, 7, 30]
            .into_iter()
            .map(|shift| frame(130, 70, gradient(shift)))
            .collect();
        for (index, image) in frames.iter().enumerate() {
            admitted(&mut journal).record(at(u64::try_from(index).unwrap() * 100), image.clone());
        }
        for (index, image) in frames.iter().enumerate() {
            let moment = at(u64::try_from(index).unwrap() * 100 + 50);
            assert_eq!(
                journal.frame_at(moment).as_ref(),
                Some(image),
                "frame {index}"
            );
        }
        assert_eq!(journal.frame_at(at(0)).as_ref(), frames.first());
        // A resolution change starts a new run of frames and leaves the old ones intact.
        let resized = frame(64, 64, gradient(3));
        admitted(&mut journal).record(at(1_000), resized.clone());
        assert_eq!(journal.frame_at(at(1_000)).as_ref(), Some(&resized));
        assert_eq!(journal.frame_at(at(150)).as_ref(), frames.get(1));
        // Sealing keeps every moment, and a frame recorded after it is stored whole.
        let whole = {
            let mut alone = Journal::new();
            admitted(&mut alone).record(at(0), resized.clone());
            alone.bytes()
        };
        let mut journal = journal.sealed();
        let held = journal.bytes();
        admitted(&mut journal).record(at(1_100), resized.clone());
        assert_eq!(
            journal.bytes() - held,
            whole,
            "a sealed journal keeps no frame to compare the next against"
        );
        let after = frame(64, 64, gradient(9));
        admitted(&mut journal).record(at(1_200), after.clone());
        assert_eq!(journal.frame_at(at(1_100)).as_ref(), Some(&resized));
        assert_eq!(journal.frame_at(at(1_200)), Some(after));
        assert_eq!(journal.frame_at(at(1_000)), Some(resized));
        assert_eq!(journal.frame_at(at(350)).as_ref(), frames.get(3));
        assert_eq!(Journal::new().frame_at(at(0)), None);
    }

    #[test]
    fn each_thinning_halves_how_often_arrivals_are_admitted() {
        // With no room past two frames, every frame recorded after the first two thins the journal.
        let mut journal = Journal::with_budget(0);
        let mut admitted = Vec::new();
        for arrival in 0_u8..9 {
            if let Some(admission) = journal.admit() {
                admitted.push(arrival);
                let moment = at(u64::from(arrival) * 200);
                admission.record(moment, quadrants([arrival; 4], 2));
            }
        }
        assert_eq!(admitted, [0, 1, 2, 4, 8]);
    }

    #[test]
    fn debugging_a_journal_never_prints_its_frames() {
        let mut journal = Journal::new();
        admitted(&mut journal).record(at(0), quadrants([0; 4], 2));
        let one = format!("{journal:?}");
        for shade in 1_u8..20 {
            let moment = at(u64::from(shade) * 200);
            admitted(&mut journal).record(moment, quadrants([shade, 0, shade, 0], 3));
        }
        let many = format!("{journal:?}");
        assert!(many.len() <= one.len() + 8, "{one}\n{many}");
    }

    proptest! {
        #[expect(
            clippy::arithmetic_side_effects,
            reason = "Moments and indices of at most 24 frames; debug overflow checks fail the test"
        )]
        #[test]
        fn thinning_keeps_every_other_frame_exact(
            changes in prop::collection::vec((0_u8..4, 0_usize..3), 1..24),
            rounds in 1_usize..4,
        ) {
            let mut journal = Journal::new();
            let mut recorded = Vec::new();
            let mut shade = [0_u8; 4];
            for (index, (quadrant, size)) in changes.into_iter().enumerate() {
                if let Some(value) = shade.get_mut(usize::from(quadrant)) {
                    *value = value.wrapping_add(40);
                }
                let image = quadrants(shade, size);
                let moment = at(u64::try_from(index).unwrap() * 100);
                admitted(&mut journal).record(moment, image.clone());
                recorded.push((moment, image));
            }
            for _ in 0..rounds {
                let before = journal.bytes();
                journal.thin();
                let count = recorded.len();
                recorded = recorded
                    .into_iter()
                    .enumerate()
                    .filter(|(index, _)| index.is_multiple_of(2) || index + 1 == count)
                    .map(|(_, frame)| frame)
                    .collect();
                prop_assert!(journal.bytes() <= before);
            }
            let moments: Vec<Moment> = recorded.iter().map(|(moment, _)| *moment).collect();
            let held: Vec<Moment> = journal.samples().iter().map(|sample| sample.at).collect();
            prop_assert_eq!(held, moments);
            for (moment, image) in &recorded {
                prop_assert_eq!(journal.frame_at(*moment), Some(image.clone()));
            }
        }

        #[test]
        fn a_budget_thins_frames_and_their_samples_together(
            arrivals in prop::collection::vec((-300_i64..=300, 0_u8..4, 0_usize..3), 1..48),
        ) {
            // A few frames' worth, so arrivals overrun it and the journal thins again and again.
            const BUDGET: usize = 1_500;
            let mut journal = Journal::with_budget(BUDGET);
            let (mut shade, mut millis) = ([0_u8; 4], 0_u64);
            let (mut first, mut last) = (None, None);
            for (delta, quadrant, size) in arrivals {
                // Capture clocks can repeat a moment or step back.
                millis = millis.saturating_add_signed(delta);
                if let Some(value) = shade.get_mut(usize::from(quadrant)) {
                    *value = value.wrapping_add(40);
                }
                let Some(admission) = journal.admit() else {
                    continue;
                };
                let image = quadrants(shade, size);
                first.get_or_insert_with(|| (at(millis), image.clone()));
                last = Some(image.clone());
                admission.record(at(millis), image);
                let held = journal.samples().len();
                prop_assert!(journal.bytes() <= BUDGET || held <= 2, "{} bytes in {held} frames", journal.bytes());
            }
            let samples = journal.samples();
            prop_assert!(samples.is_sorted_by(|earlier, later| earlier.at < later.at));
            for sample in samples {
                let shown = journal.frame_at(sample.at).unwrap();
                prop_assert!(Signature::of(&shown) == sample.signature, "at {}", sample.at.as_millis());
            }
            let (first_at, first_image) = first.unwrap();
            prop_assert_eq!(samples.first().map(|sample| sample.at), Some(first_at));
            prop_assert_eq!(journal.frame_at(first_at), Some(first_image));
            prop_assert_eq!(journal.frame_at(samples.last().unwrap().at), last);
        }

        #[expect(
            clippy::arithmetic_side_effects,
            reason = "Noise mixed from coordinates under 150; debug overflow checks fail the test"
        )]
        #[test]
        fn rebuilding_is_lossless_for_any_size(width in 1_u32..150, height in 1_u32..150, seed in 0_u8..=255) {
            let noise = move |x: usize, y: usize| {
                let mixed = x * 31 + y * 17 + usize::from(seed);
                let value = u8::try_from(mixed % 251).unwrap();
                [value, value.wrapping_mul(3), seed, 255]
            };
            let image = frame(width, height, noise);
            let mut journal = Journal::new();
            admitted(&mut journal).record(at(5), image.clone());
            prop_assert_eq!(journal.frame_at(at(5)), Some(image));
        }
    }
}
