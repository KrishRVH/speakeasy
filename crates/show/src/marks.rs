//! Strokes drawn together and pins dropped, numbered in one sequence, and what they look like.

mod stroke;

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
    /// A straight stroke with a short head at one end, drawn in one go or with a stroke or two
    /// more.
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
    /// The smallest normalized rectangle holding what the ink marks, or a pin's point. A stroke
    /// that goes round something after a lead-in or before a tail marks only its loop.
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
            let (shape, bounds) = read(members, surface)?;
            Some(Mark {
                number: 0,
                shape,
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
    /// The part of `bounds` the stroke marks.
    region: Rect,
    /// Width plus height in pixels; the largest stroke is the mark's main one.
    size: f32,
}

/// What `strokes` look like together in the pixels of `surface`, and the region they mark.
fn read(strokes: &[Stroke], surface: Size) -> Option<(Shape, Rect)> {
    let mut measured: Vec<Measured<'_>> = strokes
        .iter()
        .filter_map(|stroke| {
            let bounds = Rect::bounding(stroke.path())?;
            let stroke::Ink { shape, region } = stroke::ink(stroke, surface)?;
            Some(Measured {
                stroke,
                shape,
                bounds,
                region,
                size: bounds.width() * surface.width + bounds.height() * surface.height,
            })
        })
        .collect();
    let region = measured.iter().map(|one| one.region).reduce(Rect::union)?;
    measured.sort_by(|one, other| other.size.total_cmp(&one.size));
    let (main, rest) = measured.split_first()?;
    let shape = arrow(main, rest, surface).unwrap_or_else(|| match main.shape {
        Shape::Circle => Shape::Circle,
        Shape::Line { .. } | Shape::Arrow { .. } if !rest.is_empty() => Shape::Scribble,
        shape if rest.iter().all(|other| other.shape.same_kind(shape)) => shape,
        _ => Shape::Scribble,
    });
    Some((shape, region))
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

#[cfg(test)]
mod tests;
