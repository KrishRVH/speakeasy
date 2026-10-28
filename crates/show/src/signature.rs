//! Compact fingerprints of captured frames, compared to find where the screen changed.

use std::{fmt, ops::Range};

use crate::{Image, Moment, Rect, convert};

const COLUMNS: usize = 128;
const ROWS: usize = 72;
const CELLS: usize = COLUMNS * ROWS;
/// Transitions compare an 8 × 6 grid of blocks, each 16 × 12 cells.
const BLOCK_COLUMNS: usize = 8;
const BLOCK_ROWS: usize = 6;
const BLOCK_WIDTH: usize = COLUMNS / BLOCK_COLUMNS;
const BLOCK_HEIGHT: usize = ROWS / BLOCK_ROWS;
/// A change that fills a few blocks counts nearly as much as one spread over the whole frame.
const LOCAL_BOOST: f32 = 1.4;
/// A cell changed when its luma moved by more than this share of the full range.
const CELL_CHANGE: f32 = 0.04;
/// Cells average at most this many samples a side, so cost does not grow with resolution.
const SAMPLES_PER_CELL_SIDE: usize = 4;

/// A captured frame and the moment it was on screen, reduced to its [`Signature`].
#[derive(Clone, Debug)]
pub struct Sample {
    /// When the frame was on screen.
    pub at: Moment,
    /// The frame's fingerprint.
    pub signature: Signature,
}

/// The mean luma of a 128 × 72 grid of cells laid over a frame of any size.
///
/// A signature costs about 9 KB, so a Show can keep one for every frame it samples without
/// keeping the frames, and every comparison keyframe selection needs runs on signatures alone.
#[derive(Clone, PartialEq, Eq)]
pub struct Signature {
    cells: Box<[u8; CELLS]>,
}

/// Leaves out the cells, a thumbnail of the user's screen.
impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signature").finish_non_exhaustive()
    }
}

impl Signature {
    /// Fingerprints `image`; cost is bounded by the grid, not by the image's resolution.
    #[must_use]
    pub(crate) fn of(image: &Image) -> Self {
        let pixels: Vec<&[[u8; 4]]> = image.rows().map(|row| row.as_chunks::<4>().0).collect();
        let column_spans = spans(convert::count(image.width()), COLUMNS);
        let mut cells = Box::new([0; CELLS]);
        for (line, rows) in cells
            .as_chunks_mut::<COLUMNS>()
            .0
            .iter_mut()
            .zip(spans(pixels.len(), ROWS))
        {
            for (cell, columns) in line.iter_mut().zip(&column_spans) {
                *cell = mean_luma(&pixels, rows.clone(), columns.clone());
            }
        }
        Self { cells }
    }

    /// The mean luma difference over the whole frame, from `0.0` for identical frames to `1.0`
    /// for black against white.
    pub(crate) fn change(&self, other: &Self) -> f32 {
        let total: f32 = self.differences(other).sum();
        total / CELLS as f32
    }

    /// How strongly the screen changed between two frames.
    ///
    /// The larger of the whole-frame change and the fourth-strongest block change, boosted: a
    /// panel, dialog, or page that changes four or more blocks registers even when the rest of the
    /// frame holds still, while changes confined to a few blocks, such as a caret or a clock, do
    /// not.
    pub(crate) fn transition(&self, other: &Self) -> f32 {
        let differences: Vec<f32> = self.differences(other).collect();
        let mut blocks = [[0.0_f32; BLOCK_COLUMNS]; BLOCK_ROWS];
        let lines = differences.as_chunks::<COLUMNS>().0;
        for (band, band_lines) in blocks.iter_mut().zip(lines.chunks(BLOCK_HEIGHT)) {
            for line in band_lines {
                for (sum, cells) in band.iter_mut().zip(line.as_chunks::<BLOCK_WIDTH>().0) {
                    *sum += cells.iter().sum::<f32>();
                }
            }
        }
        let sums = blocks.as_flattened_mut();
        sums.sort_unstable_by(f32::total_cmp);
        let fourth = sums.iter().rev().nth(3).copied().unwrap_or_default()
            / (BLOCK_WIDTH * BLOCK_HEIGHT) as f32;
        self.change(other).max((fourth * LOCAL_BOOST).min(1.0))
    }

    /// The share of the cells under `region`, a rectangle in coordinates normalized to the frame,
    /// whose luma changed.
    pub(crate) fn region_change(&self, other: &Self, region: Rect) -> f32 {
        let rows = cell_range(region.top, region.bottom, ROWS);
        let columns = cell_range(region.left, region.right, COLUMNS);
        let changed = self
            .region(&rows, &columns)
            .zip(other.region(&rows, &columns))
            .filter(|&(one, another)| difference(one, another) > CELL_CHANGE)
            .count();
        changed as f32 / rows.len().saturating_mul(columns.len()).max(1) as f32
    }

    fn region(&self, rows: &Range<usize>, columns: &Range<usize>) -> impl Iterator<Item = u8> {
        self.cells
            .as_chunks::<COLUMNS>()
            .0
            .get(rows.clone())
            .unwrap_or_default()
            .iter()
            .flat_map(|line| {
                line.get(columns.clone())
                    .unwrap_or_default()
                    .iter()
                    .copied()
            })
    }

    fn differences(&self, other: &Self) -> impl Iterator<Item = f32> {
        self.cells
            .iter()
            .zip(other.cells.iter())
            .map(|(&one, &another)| difference(one, another))
    }
}

/// Splits `length` pixels into `parts` consecutive spans of at least one pixel each, so a frame
/// smaller than the grid repeats its pixels across cells instead of leaving cells empty.
fn spans(length: usize, parts: usize) -> Vec<Range<usize>> {
    let scale = length as f32 / parts as f32;
    (0..parts)
        .map(|part| {
            let start = convert::count_floor(part as f32 * scale).min(length.saturating_sub(1));
            let end = convert::count_floor((part as f32 + 1.0) * scale)
                .clamp(start.saturating_add(1), length.max(1));
            start..end
        })
        .collect()
}

/// The cells a normalized interval covers, at least one.
fn cell_range(low: f32, high: f32, cells: usize) -> Range<usize> {
    let last = cells.saturating_sub(1);
    let start = convert::count_floor(low.clamp(0.0, 1.0) * cells as f32).min(last);
    let end = convert::count_ceil(high.clamp(0.0, 1.0) * cells as f32)
        .clamp(start.saturating_add(1), cells);
    start..end
}

/// How far apart two cells' luma lies, from `0.0` when equal to `1.0` for black against white.
fn difference(one: u8, another: u8) -> f32 {
    f32::from(one.abs_diff(another)) / 255.0
}

/// Rec. 709 luma averaged over a grid of up to [`SAMPLES_PER_CELL_SIDE`]² of the `pixels` in
/// `rows` and `columns`.
fn mean_luma(pixels: &[&[[u8; 4]]], rows: Range<usize>, columns: Range<usize>) -> u8 {
    let step = |range: &Range<usize>| range.len().div_ceil(SAMPLES_PER_CELL_SIDE).max(1);
    let (row_step, column_step) = (step(&rows), step(&columns));
    let (mut sum, mut count) = (0.0_f32, 0.0_f32);
    for row in pixels
        .get(rows)
        .unwrap_or_default()
        .iter()
        .step_by(row_step)
    {
        for &[red, green, blue, _] in row
            .get(columns.clone())
            .unwrap_or_default()
            .iter()
            .step_by(column_step)
        {
            sum += 0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue);
            count += 1.0;
        }
    }
    convert::channel(sum / count.max(1.0))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A uniformly gray frame.
    fn gray(level: u8, width: u32, height: u32) -> Image {
        Image::filled(width, height, [level; 3]).unwrap()
    }

    pub(crate) fn signature(level: u8) -> Signature {
        Signature::of(&gray(level, 2, 2))
    }

    #[test]
    fn change_is_normalized_and_independent_of_resolution() {
        assert!(signature(0).change(&signature(0)).abs() < f32::EPSILON);
        assert!((signature(0).change(&signature(255)) - 1.0).abs() < 1e-6);
        let half = signature(0).change(&signature(128));
        assert!((half - 128.0 / 255.0).abs() < 1e-3);
        assert_eq!(Signature::of(&gray(128, 1920, 1080)), signature(128));
        assert_eq!(Signature::of(&gray(128, 3, 1)), signature(128));
    }

    #[test]
    fn debugging_a_signature_never_prints_the_screen() {
        assert_eq!(
            format!("{:?}", signature(0)),
            format!("{:?}", signature(255))
        );
    }

    /// The signature of a gray frame with one lighter rectangle, given in pixels.
    fn with_patch(width: u32, height: u32, patch: [f32; 4]) -> Signature {
        let mut image = gray(40, width, height);
        let [left, top, right, bottom] = patch;
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(255, 255, 255, 255);
        image.as_mut().fill_rect(
            tiny_skia::Rect::from_ltrb(left, top, right, bottom).unwrap(),
            &paint,
            tiny_skia::Transform::identity(),
            None,
        );
        Signature::of(&image)
    }

    #[test]
    fn a_cursor_sized_change_is_not_a_transition() {
        let base = Signature::of(&gray(40, 640, 360));
        // A cursor-sized square straddling a block corner.
        let cursor = with_patch(640, 360, [314.0, 174.0, 326.0, 186.0]);
        assert!(
            base.transition(&cursor) < 0.08,
            "{}",
            base.transition(&cursor)
        );
        // A panel opening over a sixth of the frame is.
        let opened = with_patch(640, 360, [0.0, 0.0, 320.0, 120.0]);
        assert!(
            base.transition(&opened) > 0.08,
            "{}",
            base.transition(&opened)
        );
        assert!(base.region_change(&opened, Rect::from([0.0, 0.0, 0.5, 0.33])) > 0.95);
        assert!(base.region_change(&opened, Rect::from([0.6, 0.5, 1.0, 1.0])) < f32::EPSILON);
    }
}
