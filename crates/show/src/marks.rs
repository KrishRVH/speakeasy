//! Strokes drawn together and pins dropped, numbered in one sequence, and what they look like.

use std::{ops::Range, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{
    Point, Rect, Span,
    recording::{Pin, Size, Stroke},
};

/// A pause longer than this between strokes starts a new mark.
pub(crate) const MARK_GAP: Duration = Duration::from_millis(1_200);
/// The most strokes one mark holds.
const MARK_STROKES: usize = 6;
/// A stroke whose extent is below this many pixels is a tap.
const TAP_PIXELS: f32 = 14.0;
/// A circle is at least this many pixels across both ways.
const CIRCLE_PIXELS: f32 = 18.0;
/// A circle's short side is at least this share of its long side. People circle wide controls
/// and lines of text with flat loops; the enclosed-area test still rejects flat zigzags.
const CIRCLE_FLATNESS: f32 = 0.15;
/// A loop's ends meet within this share of its long side.
const LOOP_CLOSURE: f32 = 0.35;
/// A loop's path runs at least this many times its long side.
const LOOP_LENGTH: f32 = 2.0;
/// A loop encloses at least this share of its bounding box.
const LOOP_FILL: f32 = 0.32;
/// A stroke is straight when its ends lie at least this share of its path length apart.
const STRAIGHTNESS: f32 = 0.88;
/// A straight stroke at least this many times wider than tall is an underline.
const UNDERLINE_ASPECT: f32 = 2.5;
/// An arrowhead's strokes cluster within this share of the shaft's length from its tip.
const ARROWHEAD_REACH: f32 = 0.35;
/// How far a number sits from what it labels, in its disc's radii: diagonally out from a pinned
/// point or a corner of the ink, and straight out past an underline's end or a line's tail.
const PIN_REACH: f32 = 0.9;
const CORNER_REACH: f32 = 0.5;
const END_REACH: f32 = 1.6;

/// What a mark looks like, which tells a reader what the user meant by it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "camelCase")]
pub enum Shape {
    /// A dab too small to be a line.
    Tap,
    /// A closed loop around something.
    Circle,
    /// A straight horizontal stroke under something.
    Underline,
    /// A straight stroke.
    Line {
        /// Where the pen went down.
        from: Point,
        /// Where it lifted.
        to: Point,
    },
    /// A straight stroke with a short stroke or two at one end.
    Arrow {
        /// The tail.
        from: Point,
        /// The head.
        to: Point,
    },
    /// Any other scribble.
    Scribble,
    /// A pin dropped without ink.
    Pin {
        /// Where it was dropped.
        point: Point,
    },
}

/// Strokes drawn in quick succession and read as one gesture, or a pin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    /// The mark's place in the Show's sequence of marks, from one, as its label shows it.
    pub number: usize,
    /// What the strokes look like together.
    #[serde(flatten)]
    pub shape: Shape,
    /// The recording's strokes that make up the mark; a pin's range is empty, placed after the
    /// strokes begun before it.
    #[serde(with = "crate::serde_range")]
    pub strokes: Range<usize>,
    /// From the first stroke's start to the last stroke's end, or the moment of a pin.
    pub span: Span,
    /// The smallest normalized rectangle holding the ink, or a pin's point.
    pub bounds: Rect,
}

impl Mark {
    /// Where the center of the mark's number goes on a surface `width × height` pixels, in its
    /// pixels, for a disc `radius` pixels in radius: beside the mark rather than over what it
    /// points at, and on the surface wherever the disc fits.
    #[must_use]
    pub fn label_center(&self, width: f32, height: f32, radius: f32) -> Point {
        let pixels = |point: Point| point.scaled(width, height);
        let Rect {
            top, right, bottom, ..
        } = self.bounds;
        let up_right = Point::new(radius, -radius);
        let center = match self.shape {
            Shape::Pin { point } => pixels(point) + up_right * PIN_REACH,
            Shape::Tap | Shape::Circle | Shape::Scribble => {
                pixels(Point::new(right, top)) + up_right * CORNER_REACH
            },
            Shape::Underline => {
                pixels(Point::new(right, top.midpoint(bottom)))
                    + Point::new(radius * END_REACH, 0.0)
            },
            Shape::Line { from, to } | Shape::Arrow { from, to } => {
                let (tail, head) = (pixels(from), pixels(to));
                let length = tail.distance(head);
                let away = if length > f32::EPSILON {
                    (tail - head) * length.recip()
                } else {
                    Point::new(-1.0, 0.0)
                };
                tail + away * (radius * END_REACH)
            },
        };
        let margin = radius + 1.0;
        Point::new(
            center.x.min(width - margin).max(margin),
            center.y.min(height - margin).max(margin),
        )
    }
}

/// The marks `strokes` and `pins` make, in the order they began, numbered from one.
///
/// Strokes drawn within [`MARK_GAP`] of each other with no pin between form one mark, up to
/// [`MARK_STROKES`] apiece, whose shape is read in the pixels of `surface`; each pin is a mark of its
/// own. Strokes and pins arrive in time order, so a later one never renumbers an earlier mark.
pub(crate) fn group(strokes: &[Stroke], pins: &[Pin], surface: Size) -> Vec<Mark> {
    let joined = |one: &Stroke, next: &Stroke| {
        let (end, start) = (one.span().end(), next.span().start());
        start.since(end) <= MARK_GAP && !pins.iter().any(|pin| (end..=start).contains(&pin.at))
    };
    let ink = strokes
        .chunk_by(joined)
        .flat_map(|run| run.chunks(MARK_STROKES))
        .filter_map(|members| {
            let span = members.iter().map(Stroke::span).reduce(Span::union)?;
            let bounds = Rect::bounding(members.iter().flat_map(Stroke::path))?;
            Some(Mark {
                number: 0,
                shape: shape(members, surface),
                strokes: strokes.subslice_range(members)?.into(),
                span,
                bounds,
            })
        });
    let pinned = pins.iter().map(|pin| {
        let before = strokes.partition_point(|stroke| stroke.span().start() <= pin.at);
        Mark {
            number: 0,
            shape: Shape::Pin { point: pin.point },
            strokes: before..before,
            span: Span::at(pin.at),
            bounds: Rect::at(pin.point),
        }
    });
    let mut marks: Vec<Mark> = ink.chain(pinned).collect();
    // Stable, so ink begun at a pin's moment keeps its place before the pin.
    marks.sort_by_key(|mark| mark.span.start());
    for (position, mark) in marks.iter_mut().enumerate() {
        mark.number = position.saturating_add(1);
    }
    marks
}

/// One stroke of a mark, with the shape it makes in pixels.
struct Measured<'a> {
    stroke: &'a Stroke,
    shape: Shape,
    /// Normalized, like the stroke's points.
    bounds: Rect,
    /// Width plus height in pixels; the largest stroke is the mark's main one.
    size: f32,
}

fn shape(strokes: &[Stroke], surface: Size) -> Shape {
    let mut measured: Vec<Measured<'_>> = strokes
        .iter()
        .filter_map(|stroke| {
            let bounds = Rect::bounding(stroke.path())?;
            Some(Measured {
                stroke,
                shape: stroke_shape(stroke, surface),
                bounds,
                size: bounds.width() * surface.width + bounds.height() * surface.height,
            })
        })
        .collect();
    measured.sort_by(|one, other| other.size.total_cmp(&one.size));
    let Some((main, rest)) = measured.split_first() else {
        return Shape::Tap;
    };
    if let Some(arrow) = arrow(main, rest, surface) {
        return arrow;
    }
    match main.shape {
        Shape::Circle => Shape::Circle,
        Shape::Line { .. } if !rest.is_empty() => Shape::Scribble,
        shape if rest.iter().all(|other| other.shape.same_kind(shape)) => shape,
        _ => Shape::Scribble,
    }
}

/// A straight main stroke with one or two short strokes clustered at one end is an arrow
/// pointing at that end, whichever direction it was drawn.
fn arrow(main: &Measured<'_>, rest: &[Measured<'_>], surface: Size) -> Option<Shape> {
    if !matches!(main.shape, Shape::Line { .. } | Shape::Underline)
        || !(1..=2).contains(&rest.len())
    {
        return None;
    }
    let pixels = |point: Point| point.scaled(surface.width, surface.height);
    let (start, end) = (main.stroke.first().point, main.stroke.last().point);
    let length = pixels(start).distance(pixels(end));
    let clustered_at = |tip: Point| {
        rest.iter().all(|other| {
            pixels(other.bounds.center()).distance(pixels(tip)) < ARROWHEAD_REACH * length
        })
    };
    [(start, end), (end, start)]
        .into_iter()
        .find(|&(_, head)| clustered_at(head))
        .map(|(from, to)| Shape::Arrow { from, to })
}

impl Shape {
    fn same_kind(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

fn stroke_shape(stroke: &Stroke, surface: Size) -> Shape {
    let pixels = |point: Point| point.scaled(surface.width, surface.height);
    let path: Vec<Point> = stroke.path().map(pixels).collect();
    let Some(bounds) = Rect::bounding(path.iter().copied()) else {
        return Shape::Tap;
    };
    let (width, height) = (bounds.width(), bounds.height());
    let (longest, shortest) = (width.max(height), width.min(height));
    let length: f32 = path
        .array_windows()
        .map(|&[one, next]| one.distance(next))
        .sum();
    let chord = pixels(stroke.first().point).distance(pixels(stroke.last().point));
    if width.hypot(height) < TAP_PIXELS {
        Shape::Tap
    } else if chord < LOOP_CLOSURE * longest
        && length > LOOP_LENGTH * longest
        && shortest >= CIRCLE_PIXELS
        && shortest > CIRCLE_FLATNESS * longest
        && enclosed_area(&path) > LOOP_FILL * width * height
    {
        Shape::Circle
    } else if chord > STRAIGHTNESS * length {
        if width > UNDERLINE_ASPECT * height {
            Shape::Underline
        } else {
            Shape::Line {
                from: stroke.first().point,
                to: stroke.last().point,
            }
        }
    } else {
        Shape::Scribble
    }
}

/// The area `path` encloses once closed by its chord, by the shoelace formula.
fn enclosed_area(path: &[Point]) -> f32 {
    let doubled: f32 = path
        .iter()
        .zip(path.iter().cycle().skip(1))
        .map(|(one, next)| one.x * next.y - next.x * one.y)
        .sum();
    0.5 * doubled.abs()
}

#[cfg(test)]
mod tests;
