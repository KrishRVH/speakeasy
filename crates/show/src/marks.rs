//! Strokes drawn together, and what they look like.

use std::{ops::Range, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{
    Point, Rect, Span,
    recording::{Size, Stroke},
};

/// A pause longer than this between strokes starts a new mark.
const MARK_GAP: Duration = Duration::from_millis(1_200);
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
}

/// Strokes drawn in quick succession, read as one gesture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    /// What the strokes look like together.
    #[serde(flatten)]
    pub shape: Shape,
    /// The recording's strokes that make up the mark.
    #[serde(with = "crate::serde_range")]
    pub strokes: Range<usize>,
    /// From the first stroke's start to the last stroke's end.
    pub span: Span,
    /// The smallest normalized rectangle holding the ink.
    pub bounds: Rect,
}

/// Groups strokes drawn within [`MARK_GAP`] of each other, up to [`MARK_STROKES`] apiece, and
/// reads each group's shape in the pixels of `surface`.
pub(crate) fn group(strokes: &[Stroke], surface: Size) -> Vec<Mark> {
    let mut groups: Vec<Range<usize>> = Vec::new();
    let mut previous_end = None;
    for (index, stroke) in strokes.iter().enumerate() {
        let continues =
            previous_end.is_some_and(|end| stroke.span().start().since(end) <= MARK_GAP);
        match groups.last_mut() {
            Some(group) if continues && group.len() < MARK_STROKES => {
                group.end = index.saturating_add(1);
            },
            _ => groups.push(index..index.saturating_add(1)),
        }
        previous_end = Some(stroke.span().end());
    }
    groups
        .into_iter()
        .filter_map(|range| {
            let members = strokes.get(range.clone())?;
            let span = members.iter().map(Stroke::span).reduce(Span::union)?;
            let bounds = Rect::bounding(members.iter().flat_map(Stroke::path))?;
            Some(Mark {
                shape: shape(members, surface),
                strokes: range,
                span,
                bounds,
            })
        })
        .collect()
}

/// One stroke measured in pixels.
struct Measured<'a> {
    stroke: &'a Stroke,
    shape: Shape,
    bounds: Rect,
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
                size: bounds
                    .width()
                    .mul_add(surface.width, bounds.height() * surface.height),
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
    if clustered_at(end) {
        Some(Shape::Arrow {
            from: start,
            to: end,
        })
    } else if clustered_at(start) {
        Some(Shape::Arrow {
            from: end,
            to: start,
        })
    } else {
        None
    }
}

impl Shape {
    fn same_kind(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

fn stroke_shape(stroke: &Stroke, surface: Size) -> Shape {
    let path: Vec<Point> = stroke
        .path()
        .map(|point| point.scaled(surface.width, surface.height))
        .collect();
    let (Some(&first), Some(&last), Some(bounds)) = (
        path.first(),
        path.last(),
        Rect::bounding(path.iter().copied()),
    ) else {
        return Shape::Tap;
    };
    let (width, height) = (bounds.width(), bounds.height());
    let (longest, shortest) = (width.max(height), width.min(height));
    let length: f32 = path
        .iter()
        .zip(path.iter().skip(1))
        .map(|(one, next)| one.distance(*next))
        .sum();
    let chord = first.distance(last);
    // Shoelace formula over the stroke closed by its chord.
    let doubled_area = path
        .iter()
        .zip(path.iter().cycle().skip(1))
        .map(|(one, next)| one.x.mul_add(next.y, -(next.x * one.y)))
        .sum::<f32>();
    if width.hypot(height) < TAP_PIXELS {
        Shape::Tap
    } else if chord < LOOP_CLOSURE * longest
        && length > LOOP_LENGTH * longest
        && shortest >= CIRCLE_PIXELS
        && shortest > CIRCLE_FLATNESS * longest
        && 0.5 * doubled_area.abs() > LOOP_FILL * width * height
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

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::*;
    use crate::{Moment, recording::InkPoint};

    const SURFACE: Size = Size {
        width: 1000.0,
        height: 1000.0,
    };

    fn stroke(points: &[[f32; 2]], at: u64) -> Stroke {
        Stroke::new(
            points
                .iter()
                .map(|&point| InkPoint {
                    point: point.into(),
                    at: Moment::from_millis(at),
                })
                .collect(),
        )
        .unwrap()
    }

    fn shape_of(points: &[[f32; 2]], surface: Size) -> Shape {
        group(&[stroke(points, 0)], surface)[0].shape
    }

    #[test]
    fn shapes_are_measured_in_pixels_of_the_surface() {
        assert_eq!(shape_of(&[[0.5, 0.5], [0.505, 0.505]], SURFACE), Shape::Tap);
        assert_eq!(
            shape_of(&[[0.1, 0.5], [0.8, 0.5]], SURFACE),
            Shape::Underline
        );
        assert_eq!(
            shape_of(&[[0.2, 0.2], [0.5, 0.6]], SURFACE),
            Shape::Line {
                from: Point::new(0.2, 0.2),
                to: Point::new(0.5, 0.6)
            }
        );
        let loop_points: Vec<[f32; 2]> = (0..=32_u8)
            .map(|step| {
                let angle = TAU * f32::from(step) / 32.0;
                [
                    0.1f32.mul_add(angle.cos(), 0.5),
                    0.2f32.mul_add(angle.sin(), 0.5),
                ]
            })
            .collect();
        let half_height = Size {
            width: 1000.0,
            height: 500.0,
        };
        assert_eq!(shape_of(&loop_points, half_height), Shape::Circle);
        // A flat loop around a wide control is still a circle; a flat zigzag is not.
        let flat: Vec<[f32; 2]> = loop_points
            .iter()
            .map(|[x, y]| [x.mul_add(2.0, -0.5), y.mul_add(0.4, 0.3)])
            .collect();
        assert_eq!(shape_of(&flat, half_height), Shape::Circle);
        let zigzag = [
            [0.1, 0.40],
            [0.7, 0.42],
            [0.1, 0.44],
            [0.7, 0.46],
            [0.12, 0.40],
        ];
        assert_eq!(shape_of(&zigzag, half_height), Shape::Scribble);
        // The same loop on a tiny surface is a tap.
        let tiny = Size {
            width: 20.0,
            height: 20.0,
        };
        assert_eq!(shape_of(&loop_points, tiny), Shape::Tap);
    }

    #[test]
    fn an_arrow_points_at_its_head_whichever_way_it_was_drawn() {
        for shaft in [[[0.2, 0.8], [0.5, 0.5]], [[0.5, 0.5], [0.2, 0.8]]] {
            let marks = group(
                &[
                    stroke(&shaft, 0),
                    stroke(&[[0.42, 0.5], [0.5, 0.5], [0.5, 0.58]], 200),
                ],
                SURFACE,
            );
            assert_eq!(marks.len(), 1);
            assert_eq!(
                marks[0].shape,
                Shape::Arrow {
                    from: Point::new(0.2, 0.8),
                    to: Point::new(0.5, 0.5)
                }
            );
            assert_eq!(
                marks[0].to_string(),
                "drew an arrow from x 0.20, y 0.80 pointing to x 0.50, y 0.50"
            );
        }
    }

    #[test]
    fn pauses_and_stroke_limits_separate_marks() {
        let strokes: Vec<Stroke> = [0, 200, 400, 600, 800, 1000, 1200, 4000]
            .into_iter()
            .map(|at| stroke(&[[0.5, 0.5]], at))
            .collect();
        let marks = group(&strokes, SURFACE);
        let ranges: Vec<_> = marks.iter().map(|mark| mark.strokes.clone()).collect();
        assert_eq!(ranges, [0..6, 6..7, 7..8]);
        assert_eq!(marks[0].to_string(), "tapped x 0.50, y 0.50 (6 strokes)");
    }

    #[test]
    fn underlines_report_their_extent_and_stroke_count() {
        let marks = group(
            &[
                stroke(&[[0.1, 0.5], [0.8, 0.5]], 0),
                stroke(&[[0.2, 0.52], [0.7, 0.52]], 200),
            ],
            SURFACE,
        );
        assert_eq!(
            marks[0].to_string(),
            "underlined x 0.10–0.80 at y 0.51 (2 strokes)"
        );
    }
}
