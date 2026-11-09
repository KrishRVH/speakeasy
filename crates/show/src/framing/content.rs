use std::ops::Range;

use super::Screen;
use crate::{Image, Rect, Size, convert, signature::spans};

const CELL: f32 = 3.0;

const SAMPLES_PER_SIDE: usize = 3;

const CONTRAST: f32 = 28.0;

const GUTTER: f32 = 12.0;

const REACH: f32 = 120.0;

const REACH_SHARE: f32 = 0.6;

const BORDER_SHARE: usize = 10;

pub(super) struct Content {
    lines: Vec<Vec<bool>>,
    columns: usize,
}

impl Content {
    pub(super) fn of(frame: &Image, surface: Size) -> Self {
        let cells = |points: f32, pixels: u32| {
            convert::count_floor((points / CELL).round()).clamp(1, convert::count(pixels).max(1))
        };
        let columns = cells(surface.width, frame.width());
        let pixels: Vec<&[[u8; 4]]> = frame.rows().map(|row| row.as_chunks::<4>().0).collect();
        let column_spans = spans(convert::count(frame.width()), columns);
        let lines = spans(pixels.len(), cells(surface.height, frame.height()))
            .into_iter()
            .map(|rows| {
                column_spans
                    .iter()
                    .map(|columns| busy(&pixels, rows.clone(), columns.clone()))
                    .collect()
            })
            .collect();
        Self { lines, columns }
    }

    pub(super) fn snap(&self, crop: Rect, screen: Screen, within: Rect) -> Rect {
        let (columns, rows) = (self.columns, self.lines.len());
        let gutter = convert::count_ceil(GUTTER / CELL);
        let across = reach(crop.width() * screen.width);
        let down = reach(crop.height() * screen.height);
        let tall = span(crop.top, crop.bottom, rows);
        let column_empty = |column: usize| {
            let lines = self.lines.get(tall.clone()).unwrap_or_default();
            sparse(
                lines.iter().map(|line| line.get(column) == Some(&true)),
                tall.len(),
            )
        };
        let (left, right) = gutters(
            span(crop.left, crop.right, columns),
            columns,
            gutter,
            across,
            column_empty,
        );
        let crop = bounded(
            crop,
            Rect {
                left: at(left, columns, crop.left),
                right: at(right, columns, crop.right),
                ..crop
            },
            within,
        );
        let wide = span(crop.left, crop.right, columns);
        let row_empty = |row: usize| {
            let line = self.lines.get(row).and_then(|line| line.get(wide.clone()));
            sparse(line.unwrap_or_default().iter().copied(), wide.len())
        };
        let (top, bottom) = gutters(tall, rows, 1, down, row_empty);
        let crop = bounded(
            crop,
            Rect {
                top: at(top, rows, crop.top),
                bottom: at(bottom, rows, crop.bottom),
                ..crop
            },
            within,
        );
        let lines = self
            .lines
            .get(span(crop.top, crop.bottom, rows))
            .unwrap_or_default();
        let (left, right) = follow(
            lines.iter().map(Vec::as_slice),
            span(crop.left, crop.right, columns),
            columns,
            gutter,
            across,
        );
        let crop = bounded(
            crop,
            Rect {
                left: at(left, columns, crop.left),
                right: at(right, columns, crop.right),
                ..crop
            },
            within,
        );
        let verticals: Vec<Vec<bool>> = span(crop.left, crop.right, columns)
            .map(|column| {
                self.lines
                    .iter()
                    .map(|line| line.get(column) == Some(&true))
                    .collect()
            })
            .collect();
        let (top, bottom) = follow(
            verticals.iter().map(Vec::as_slice),
            span(crop.top, crop.bottom, rows),
            rows,
            gutter,
            down,
        );
        bounded(
            crop,
            Rect {
                top: at(top, rows, crop.top),
                bottom: at(bottom, rows, crop.bottom),
                ..crop
            },
            within,
        )
    }
}

fn bounded(crop: Rect, moved: Rect, within: Rect) -> Rect {
    Rect {
        left: moved.left.max(within.left).min(crop.left),
        top: moved.top.max(within.top).min(crop.top),
        right: moved.right.min(within.right).max(crop.right),
        bottom: moved.bottom.min(within.bottom).max(crop.bottom),
    }
}

fn gutters(
    cells: Range<usize>,
    count: usize,
    gutter: usize,
    reach: usize,
    empty: impl Fn(usize) -> bool,
) -> (Option<usize>, Option<usize>) {
    (
        outward(cells.start, Side::Low, count, gutter, reach, &empty),
        outward(cells.end, Side::High, count, gutter, reach, &empty),
    )
}

fn follow<'a>(
    lines: impl Iterator<Item = &'a [bool]> + Clone,
    cells: Range<usize>,
    count: usize,
    gutter: usize,
    reach: usize,
) -> (Option<usize>, Option<usize>) {
    let low = lines
        .clone()
        .filter_map(|line| line_end(line, cells.start, Side::Low, reach, gutter))
        .min()
        .map(|start| start.saturating_sub(gutter));
    let high = lines
        .filter_map(|line| line_end(line, cells.end, Side::High, reach, gutter))
        .max()
        .map(|end| end.saturating_add(gutter).saturating_add(1).min(count));
    (low, high)
}

fn sparse(cells: impl Iterator<Item = bool>, length: usize) -> bool {
    cells.filter(|&busy| busy).count() <= length / BORDER_SHARE
}

fn at(edge: Option<usize>, cells: usize, otherwise: f32) -> f32 {
    edge.map_or(otherwise, |edge| edge as f32 / cells.max(1) as f32)
}

#[derive(Clone, Copy)]
enum Side {
    Low,
    High,
}

fn reach(size: f32) -> usize {
    convert::count_ceil(REACH.max(REACH_SHARE * size) / CELL)
}

fn outward(
    edge: usize,
    side: Side,
    lines: usize,
    gutter: usize,
    reach: usize,
    empty: impl Fn(usize) -> bool,
) -> Option<usize> {
    let clear = |boundary: usize| match side {
        Side::Low => {
            boundary == 0
                || (boundary..boundary.saturating_add(gutter))
                    .all(|line| line >= lines || empty(line))
        },
        Side::High => boundary >= lines || (boundary.saturating_sub(gutter)..boundary).all(&empty),
    };
    if clear(edge) {
        return None;
    }
    (1..=reach)
        .map_while(|distance| match side {
            Side::Low => edge.checked_sub(distance),
            Side::High => edge
                .checked_add(distance)
                .filter(|&boundary| boundary <= lines),
        })
        .find(|&boundary| clear(boundary))
}

fn line_end(
    line: &[bool],
    boundary: usize,
    side: Side,
    reach: usize,
    gutter: usize,
) -> Option<usize> {
    let beyond = |distance: usize| match side {
        Side::Low => boundary.checked_sub(distance.saturating_add(1)),
        Side::High => boundary
            .checked_add(distance)
            .filter(|&cell| cell < line.len()),
    };
    let within = |distance: usize| match side {
        Side::Low => boundary.checked_add(distance),
        Side::High => boundary.checked_sub(distance.saturating_add(1)),
    };
    let busy = |cell: Option<usize>| cell.and_then(|cell| line.get(cell)) == Some(&true);
    let crosses = (0..gutter).any(|distance| busy(within(distance)))
        && (0..gutter).any(|distance| busy(beyond(distance)));
    if !crosses {
        return None;
    }
    let (mut last, mut blank) = (0_usize, 0_usize);
    for distance in 0..reach {
        let Some(cell) = beyond(distance) else {
            return beyond(last);
        };
        if busy(Some(cell)) {
            (last, blank) = (distance, 0);
        } else {
            blank = blank.saturating_add(1);
            if blank >= gutter {
                return beyond(last);
            }
        }
    }
    None
}

fn span(low: f32, high: f32, cells: usize) -> Range<usize> {
    let start = convert::count_floor(low.clamp(0.0, 1.0) * cells as f32).min(cells);
    let end = convert::count_ceil(high.clamp(0.0, 1.0) * cells as f32).clamp(start, cells);
    start..end
}

fn busy(pixels: &[&[[u8; 4]]], rows: Range<usize>, columns: Range<usize>) -> bool {
    let step = |range: &Range<usize>| range.len().div_ceil(SAMPLES_PER_SIDE).max(1);
    let luma = |x: usize, y: usize| {
        let [red, green, blue, _] = *pixels.get(y)?.get(x)?;
        Some(0.3 * f32::from(red) + 0.6 * f32::from(green) + 0.1 * f32::from(blue))
    };
    let column_step = step(&columns);
    rows.clone().step_by(step(&rows)).any(|y| {
        columns.clone().step_by(column_step).any(|x| {
            luma(x, y).is_some_and(|here| {
                [luma(x.saturating_add(1), y), luma(x, y.saturating_add(1))]
                    .into_iter()
                    .flatten()
                    .any(|next| (here - next).abs() >= CONTRAST)
            })
        })
    })
}

#[cfg(test)]
mod tests;
