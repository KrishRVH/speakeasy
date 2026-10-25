//! Contact sheets: keyframes in readable pages sized for a reading model.

use std::ops::Range;

use tiny_skia::{PixmapPaint, Transform};

use crate::{
    Error, Image, Keyframe, Point, Rect, Typeface, convert,
    draw::{self, Corner},
    parallel,
};

const PADDING: u32 = 6;
const GUTTER: u32 = 6;
const BACKGROUND: [u8; 3] = [28, 29, 34];
/// Vision models read about this many pixels on the long edge before downscaling, and about this
/// many in all.
const LONGEST_EDGE: f32 = 1_600.0;
const MOST_PIXELS: f32 = 1_560_000.0;
/// The most frames one row holds.
const ROW_FRAMES: usize = 6;
/// More frames start another sheet rather than shrinking every thumbnail.
pub(crate) const PAGE_FRAMES: usize = 6;
/// Layout treats frames as no narrower or wider than these aspects, so an extreme crop still gets
/// a usable cell; the frame then keeps its own proportions inside it.
const NARROWEST: f32 = 0.25;
const WIDEST: f32 = 4.5;
/// The sheet never narrows below this, however many rows it holds.
const SMALLEST_INNER: u32 = 64;
/// Fixed padding and gutters keep a sheet from shrinking in proportion, so fitting it within the
/// limits can take a few passes.
const FIT_PASSES: usize = 16;
/// Two frames whose aspects add up past this sit in rows of their own.
const PAIR_SPLIT: f32 = 3.6;
/// Among three or more frames, one takes a row to itself only when at least this wide.
const WIDE_ALONE: f32 = 1.85;
/// Frames shown smaller or larger than these multiples of the median area are penalized.
const SMALL_AREA: f32 = 0.58;
const LARGE_AREA: f32 = 2.3;
/// A frame narrower than this share of the sheet's width is cramped.
const NARROW_SHARE: f32 = 0.16;

/// One keyframe's place on the sheet, in whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

/// Lays the keyframes out in justified rows and draws them, badged, on one image.
pub(crate) fn render(
    keyframes: &[Keyframe],
    frames: &[(Image, Corner)],
    typeface: &Typeface,
) -> Result<Image, Error> {
    let aspects: Vec<f32> = frames
        .iter()
        .map(|(image, _)| image.aspect().clamp(NARROWEST, WIDEST))
        .collect();
    let widths: Vec<f32> = frames
        .iter()
        .map(|(image, _)| image.width() as f32)
        .collect();
    let ((width, height), cells) = layout(&aspects, &widths);
    let mut sheet = Image::filled(width, height, BACKGROUND)?;
    let smallest = cells.iter().fold((f32::MAX, f32::MAX), |smallest, cell| {
        (
            smallest.0.min(cell.width as f32),
            smallest.1.min(cell.height as f32),
        )
    });
    let badge_scale = 0.9_f32
        .min(smallest.0 / 120.0)
        .min(smallest.1 / 72.0)
        .max(0.5);
    let fits: Vec<(&Image, (u32, u32))> = frames
        .iter()
        .zip(&cells)
        .map(|((image, _), cell)| (image, fitted(image, *cell)))
        .collect();
    let scaled = parallel::map(&fits, |(image, (width, height))| {
        image.resized(*width, *height)
    });
    let mut canvas = sheet.as_mut();
    for (((keyframe, (_, corner)), cell), scaled) in
        keyframes.iter().zip(frames).zip(&cells).zip(scaled)
    {
        if let Some(scaled) = scaled {
            let (width, height) = (scaled.width(), scaled.height());
            let left = cell
                .left
                .saturating_add(cell.width.saturating_sub(width) / 2);
            let top = cell
                .top
                .saturating_add(cell.height.saturating_sub(height) / 2);
            canvas.draw_pixmap(
                i32::try_from(left).unwrap_or(i32::MAX),
                i32::try_from(top).unwrap_or(i32::MAX),
                scaled.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
        let bounds = Rect::from_origin(
            Point::new(cell.left as f32, cell.top as f32),
            cell.width as f32,
            cell.height as f32,
        );
        draw::badge(
            &mut canvas,
            bounds,
            keyframe,
            badge_scale,
            *corner,
            typeface,
        );
    }
    Ok(sheet)
}

/// The frame's size in its cell. Planning sized the cell for the frame, so the frame fills it,
/// unless its aspect was clamped for layout; then it keeps its proportions and is centered.
fn fitted(image: &Image, cell: Cell) -> (u32, u32) {
    let scale =
        (cell.width as f32 / image.width() as f32).min(cell.height as f32 / image.height() as f32);
    let width = convert::pixels(image.width() as f32 * scale).clamp(1, cell.width.max(1));
    let height = convert::pixels(image.height() as f32 * scale).clamp(1, cell.height.max(1));
    if cell.width.abs_diff(width) <= 2 && cell.height.abs_diff(height) <= 2 {
        (cell.width, cell.height)
    } else {
        (width, height)
    }
}

/// The sheet's size and each frame's cell, given frames' aspect ratios and native widths.
///
/// Frames keep their native size where the sheet allows, small crops still get room, and the
/// whole sheet stays within [`LONGEST_EDGE`] and [`MOST_PIXELS`].
fn layout(aspects: &[f32], widths: &[f32]) -> ((u32, u32), Vec<Cell>) {
    let rows = plan(aspects);
    let row_aspects: Vec<f32> = rows.iter().map(|row| sum(aspects, row)).collect();
    let sheet_aspect = row_aspects
        .iter()
        .map(|aspect| aspect.recip())
        .sum::<f32>()
        .recip();
    let native = rows
        .iter()
        .map(|row| sum(widths, row) + gaps(row) as f32)
        .fold(0.0, f32::max);
    let least = if aspects.len() == 1 { 640.0 } else { 960.0 };
    let usable = (PADDING as f32).mul_add(-2.0, LONGEST_EDGE);
    let widest = if sheet_aspect >= 1.0 {
        usable
    } else {
        usable * sheet_aspect
    };
    let mut inner = convert::pixels(native.max(least).min(widest)).max(SMALLEST_INNER);
    for _ in 0..FIT_PASSES {
        let placed = place(aspects, &rows, &row_aspects, inner);
        let (width, height) = (placed.0.0 as f32, placed.0.1 as f32);
        let fit = (LONGEST_EDGE / width.max(height)).min((MOST_PIXELS / (width * height)).sqrt());
        if fit >= 1.0 || inner <= SMALLEST_INNER {
            return placed;
        }
        inner = convert::pixels((inner as f32 * fit).floor())
            .saturating_sub(1)
            .max(SMALLEST_INNER);
    }
    place(aspects, &rows, &row_aspects, inner)
}

/// The gutters between a row's frames, in pixels.
fn gaps(row: &Range<usize>) -> u32 {
    GUTTER.saturating_mul(u32::try_from(row.len().saturating_sub(1)).unwrap_or(u32::MAX))
}

/// Cells for justified rows `inner` pixels wide. Heights round down, so a sheet measured once is
/// never larger when drawn.
fn place(
    aspects: &[f32],
    rows: &[Range<usize>],
    row_aspects: &[f32],
    inner: u32,
) -> ((u32, u32), Vec<Cell>) {
    let mut cells = Vec::with_capacity(aspects.len());
    let mut top = PADDING;
    for (row, row_aspect) in rows.iter().zip(row_aspects) {
        let available = inner.saturating_sub(gaps(row));
        let height = convert::pixels((available as f32 / row_aspect).floor()).max(1);
        let mut covered = 0.0;
        let mut left = PADDING;
        let members = aspects.get(row.clone()).unwrap_or_default();
        for (position, aspect) in members.iter().enumerate() {
            let start = convert::pixels(available as f32 * covered / row_aspect);
            covered += aspect;
            let end = if position.saturating_add(1) == members.len() {
                available
            } else {
                convert::pixels(available as f32 * covered / row_aspect)
            };
            let width = end.saturating_sub(start).max(1);
            cells.push(Cell {
                left,
                top,
                width,
                height,
            });
            left = left.saturating_add(width).saturating_add(GUTTER);
        }
        top = top.saturating_add(height).saturating_add(GUTTER);
    }
    let width = inner.saturating_add(PADDING.saturating_mul(2));
    let height = top.saturating_sub(GUTTER).saturating_add(PADDING);
    ((width, height), cells)
}

fn sum(values: &[f32], range: &Range<usize>) -> f32 {
    values.get(range.clone()).unwrap_or_default().iter().sum()
}

/// Splits frames, in order, into justified rows of one to six.
///
/// Two frames share a row unless together they are too wide. For three or more, every split is
/// scored and the cheapest wins. The score keeps the sheet's aspect near a vision model's sweet
/// spot, keeps every frame's displayed area near the others' so wide and tall crops both stay
/// legible, and avoids cramped rows; a frame gets a row to itself only when it is very wide.
fn plan(aspects: &[f32]) -> Vec<Range<usize>> {
    match *aspects {
        [] => Vec::new(),
        [_] => rows(&[1]),
        [one, two] if one + two > PAIR_SPLIT => rows(&[1, 1]),
        [_, _] => rows(&[2]),
        _ => {
            let scorer = Scorer::new(aspects);
            let mut best = (f32::INFINITY, Vec::new());
            let mut rows = Vec::new();
            search(&scorer, 0, &mut rows, &mut best);
            best.1
        },
    }
}

/// Consecutive rows of the given lengths.
fn rows(lengths: &[usize]) -> Vec<Range<usize>> {
    lengths
        .iter()
        .scan(0, |start: &mut usize, &length| {
            let row = *start..start.saturating_add(length);
            *start = row.end;
            Some(row)
        })
        .collect()
}

fn search(
    scorer: &Scorer<'_>,
    start: usize,
    rows: &mut Vec<Range<usize>>,
    best: &mut (f32, Vec<Range<usize>>),
) {
    let count = scorer.aspects.len();
    if start >= count {
        let cost = scorer.cost(rows);
        if cost < best.0 {
            *best = (cost, rows.clone());
        }
        return;
    }
    let most = ROW_FRAMES.min(count.saturating_sub(start));
    for length in 1..=most {
        let alone_and_narrow = length == 1
            && scorer
                .aspects
                .get(start)
                .is_some_and(|aspect| *aspect < WIDE_ALONE);
        if alone_and_narrow {
            continue;
        }
        let end = start.saturating_add(length);
        rows.push(start..end);
        search(scorer, end, rows, best);
        rows.pop();
    }
}

struct Scorer<'a> {
    aspects: &'a [f32],
    mean_aspect: f32,
    target_sheet: f32,
    target_row: f32,
}

impl<'a> Scorer<'a> {
    fn new(aspects: &'a [f32]) -> Self {
        let total: f32 = aspects.iter().sum();
        let mean_aspect = total / aspects.len() as f32;
        // Portrait frames, such as phones, read best on a squarer sheet.
        let target_sheet = if mean_aspect < 0.7 { 1.05 } else { 1.25 };
        Self {
            aspects,
            mean_aspect,
            target_sheet,
            target_row: (total * target_sheet).sqrt().max(mean_aspect),
        }
    }

    /// The weights are Scrawl's, tuned by eye against simulated captures.
    fn cost(&self, rows: &[Range<usize>]) -> f32 {
        let count = self.aspects.len() as f32;
        let row_count = rows.len() as f32;
        let row_aspects: Vec<f32> = rows.iter().map(|row| sum(self.aspects, row)).collect();
        let sheet_aspect = row_aspects
            .iter()
            .map(|aspect| aspect.recip())
            .sum::<f32>()
            .recip();
        let sheet_penalty = (sheet_aspect / self.target_sheet).ln().powi(2) * 1.8;

        // A frame's displayed area is proportional to its aspect over its row's aspect squared.
        let areas: Vec<f32> = rows
            .iter()
            .zip(&row_aspects)
            .flat_map(|(row, row_aspect)| {
                self.aspects
                    .get(row.clone())
                    .unwrap_or_default()
                    .iter()
                    .map(move |aspect| aspect / row_aspect.powi(2))
            })
            .collect();
        let mean_log_area = areas.iter().map(|area| area.ln()).sum::<f32>() / count;
        let area_variance = areas
            .iter()
            .map(|area| (area.ln() - mean_log_area).powi(2))
            .sum::<f32>()
            / count;
        let mut sorted = areas.clone();
        sorted.sort_by(f32::total_cmp);
        let middle = |index: usize| sorted.get(index).copied().unwrap_or_default();
        let median = f32::midpoint(
            middle(sorted.len().saturating_sub(1) / 2),
            middle(sorted.len() / 2),
        );
        let outliers: f32 = areas
            .iter()
            .map(|area| {
                let ratio = area / median;
                let small = if ratio < SMALL_AREA {
                    ((SMALL_AREA - ratio) / SMALL_AREA).powi(2) * 18.0
                } else {
                    0.0
                };
                let large = if ratio > LARGE_AREA {
                    ((ratio - LARGE_AREA) / LARGE_AREA).powi(2) * 4.0
                } else {
                    0.0
                };
                small + large
            })
            .sum();

        let mean_log_row = row_aspects.iter().map(|aspect| aspect.ln()).sum::<f32>() / row_count;
        let row_variance = row_aspects
            .iter()
            .map(|aspect| (aspect.ln() - mean_log_row).powi(2))
            .sum::<f32>();
        let row_penalty = row_aspects
            .iter()
            .map(|aspect| (aspect / self.target_row).ln().powi(2))
            .sum::<f32>();
        let mut cramping = (row_count - 4.0).max(0.0) * 2.5;
        let mut tie_break = 0.0;
        for (position, (row, row_aspect)) in rows.iter().zip(&row_aspects).enumerate() {
            for aspect in self.aspects.get(row.clone()).unwrap_or_default() {
                let share = aspect / row_aspect * sheet_aspect.min(1.0);
                if share < NARROW_SHARE {
                    cramping = ((NARROW_SHARE - share) / NARROW_SHARE)
                        .powi(2)
                        .mul_add(8.0, cramping);
                }
            }
            let columns = row.len() as f32;
            if self.mean_aspect > 0.75 {
                cramping = (columns - 4.0).max(0.0).mul_add(2.0, cramping);
            }
            if self.mean_aspect > 1.15 {
                cramping = (columns - 3.0).max(0.0).mul_add(2.0, cramping);
            }
            tie_break = (position as f32 * 1e-4).mul_add(columns, tie_break);
        }
        let rows = (row_variance / row_count).mul_add(0.6, row_penalty / row_count * 0.4);
        area_variance.mul_add(8.0, sheet_penalty + outliers + rows + cramping + tie_break)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn lengths(aspects: &[f32]) -> Vec<usize> {
        plan(aspects).iter().map(Range::len).collect()
    }

    #[test]
    fn screens_fill_balanced_rows() {
        assert_eq!(lengths(&[]), Vec::<usize>::new());
        assert_eq!(lengths(&[1.6]), [1]);
        assert_eq!(lengths(&[1.6, 1.6]), [2]);
        assert_eq!(lengths(&[2.0, 2.0]), [1, 1]);
        assert_eq!(lengths(&[1.6; 3]), [3]);
        assert_eq!(lengths(&[1.6; 6]), [2, 2, 2]);
        assert_eq!(lengths(&[1.6; 12]), [3, 3, 3, 3]);
        // A very wide frame takes a row to itself; phones sit four to a row.
        assert_eq!(lengths(&[2.4, 1.0, 1.6, 0.8, 1.6]), [1, 2, 2]);
        assert_eq!(lengths(&[0.46; 8]), [4, 4]);
    }

    #[test]
    fn layout_stays_within_what_a_model_reads() {
        // The first case lays out tall against the longest edge; the other two need more than one
        // fitting pass (see `FIT_PASSES`).
        for aspects in [
            &[0.25, 0.25, 4.5, 4.5, 3.0, 0.46, 4.5, 2.4, 0.56, 2.0][..],
            &[3.0, 4.5, 4.5, 2.0, 0.56, 1.33, 4.5, 4.5, 4.5],
            &[1.6; 12],
        ] {
            let ((width, height), _) = layout(aspects, &vec![2560.0; aspects.len()]);
            assert!(width.max(height) <= 1_600, "{width}×{height}");
            assert!(
                width as f32 * height as f32 <= MOST_PIXELS,
                "{width}×{height}"
            );
        }
        let (_, cells) = layout(&[1.6; 12], &[2560.0; 12]);
        assert_eq!(cells.len(), 12);
        // Rows are justified: the last cell of every row ends at the same right edge.
        let right_edges: Vec<u32> = cells
            .windows(2)
            .filter(|pair| pair[1].top != pair[0].top)
            .map(|pair| pair[0].left.saturating_add(pair[0].width))
            .chain(
                cells
                    .last()
                    .map(|cell| cell.left.saturating_add(cell.width)),
            )
            .collect();
        assert_eq!(right_edges.len(), 4);
        assert!(
            right_edges.windows(2).all(|pair| pair[0] == pair[1]),
            "{right_edges:?}"
        );
    }

    #[test]
    fn a_clamped_frame_keeps_its_proportions_in_its_cell() {
        let cell = Cell {
            left: 0,
            top: 0,
            width: 450,
            height: 100,
        };
        let strip = Image::filled(1_000, 100, [0; 3]).unwrap();
        assert_eq!(fitted(&strip, cell), (450, 45));
        let screen = Image::filled(900, 199, [0; 3]).unwrap();
        assert_eq!(fitted(&screen, cell), (450, 100));
    }

    proptest! {
        #[test]
        fn every_frame_gets_exactly_one_cell_in_order(
            aspects in prop::collection::vec(0.25_f32..4.5, 1..=12),
        ) {
            let rows = plan(&aspects);
            let flattened: Vec<usize> = rows.iter().flat_map(Clone::clone).collect();
            prop_assert_eq!(flattened, (0..aspects.len()).collect::<Vec<_>>());
            prop_assert!(rows.iter().all(|row| (1..=ROW_FRAMES).contains(&row.len())));
            let widths = vec![2560.0; aspects.len()];
            let ((width, height), cells) = layout(&aspects, &widths);
            prop_assert!(width.max(height) as f32 <= LONGEST_EDGE, "{}×{}", width, height);
            prop_assert!(width as f32 * height as f32 <= MOST_PIXELS, "{}×{}", width, height);
            prop_assert_eq!(cells.len(), aspects.len());
            for cell in cells {
                prop_assert!(cell.width >= 1 && cell.height >= 1);
                prop_assert!(cell.left.saturating_add(cell.width) <= width);
                prop_assert!(cell.top.saturating_add(cell.height) <= height);
            }
        }
    }
}
