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
    pub fn label(&self, width: f32, height: f32, radius: f32) -> Point {
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
    let mut groups: Vec<Range<usize>> = Vec::new();
    let mut previous_end = None;
    for (index, stroke) in strokes.iter().enumerate() {
        let start = stroke.span().start();
        let continues = previous_end.is_some_and(|end| {
            start.since(end) <= MARK_GAP && !pins.iter().any(|pin| (end..=start).contains(&pin.at))
        });
        match groups.last_mut() {
            Some(group) if continues && group.len() < MARK_STROKES => {
                group.end = index.saturating_add(1);
            },
            _ => groups.push(index..index.saturating_add(1)),
        }
        previous_end = Some(stroke.span().end());
    }
    let mut marks: Vec<Mark> = groups
        .into_iter()
        .filter_map(|range| {
            let members = strokes.get(range.clone())?;
            let span = members.iter().map(Stroke::span).reduce(Span::union)?;
            let bounds = Rect::bounding(members.iter().flat_map(Stroke::path))?;
            Some(Mark {
                number: 0,
                shape: shape(members, surface),
                strokes: range,
                span,
                bounds,
            })
        })
        .collect();
    marks.extend(pins.iter().map(|pin| {
        let before = strokes.partition_point(|stroke| stroke.span().start() <= pin.at);
        Mark {
            number: 0,
            shape: Shape::Pin { point: pin.point },
            strokes: before..before,
            span: Span::at(pin.at),
            bounds: Rect::from_origin(pin.point, 0.0, 0.0),
        }
    }));
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
        .map(|(one, next)| one.x * next.y - next.x * one.y)
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
        group(&[stroke(points, 0)], &[], surface)[0].shape
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
                [0.5 + 0.1 * angle.cos(), 0.5 + 0.2 * angle.sin()]
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
            .map(|[x, y]| [x * 2.0 - 0.5, y * 0.4 + 0.3])
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
                &[],
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
                "① drew an arrow from x 0.20, y 0.80 pointing to x 0.50, y 0.50"
            );
        }
    }

    #[test]
    fn pauses_and_stroke_limits_separate_marks() {
        let strokes: Vec<Stroke> = [0, 200, 400, 600, 800, 1000, 1200, 4000]
            .into_iter()
            .map(|at| stroke(&[[0.5, 0.5]], at))
            .collect();
        let marks = group(&strokes, &[], SURFACE);
        let ranges: Vec<_> = marks.iter().map(|mark| mark.strokes.clone()).collect();
        assert_eq!(ranges, [0..6, 6..7, 7..8]);
        assert_eq!(marks[0].to_string(), "① tapped x 0.50, y 0.50 (6 strokes)");
    }

    #[test]
    fn pins_and_ink_share_one_sequence_in_the_order_they_began() {
        let strokes = [
            stroke(&[[0.1, 0.1], [0.3, 0.1]], 0),
            stroke(&[[0.5, 0.5], [0.7, 0.5]], 5_000),
        ];
        let pin = |at: u64, x: f32| Pin {
            at: Moment::from_millis(at),
            point: Point::new(x, 0.4),
        };
        let pins = [pin(2_000, 0.4), pin(9_000, 0.8)];
        let marks = group(&strokes, &pins, SURFACE);
        let sequence: Vec<_> = marks
            .iter()
            .map(|mark| (mark.number, mark.strokes.clone()))
            .collect();
        assert_eq!(sequence, [(1, 0..1), (2, 1..1), (3, 1..2), (4, 2..2)]);
        assert_eq!(
            marks[1].shape,
            Shape::Pin {
                point: Point::new(0.4, 0.4)
            }
        );
        assert_eq!(marks[3].to_string(), "④ pinned x 0.80, y 0.40");
        let quick = [
            stroke(&[[0.1, 0.1], [0.3, 0.1]], 0),
            stroke(&[[0.5, 0.5], [0.7, 0.5]], 800),
        ];
        let ranges: Vec<_> = group(&quick, &[pin(500, 0.4)], SURFACE)
            .iter()
            .map(|mark| mark.strokes.clone())
            .collect();
        assert_eq!(
            ranges,
            [0..1, 1..1, 1..2],
            "a pin between strokes drawn close together ends a mark"
        );
        // Marks made later never renumber earlier ones, so live labels match the bundle's.
        assert_eq!(group(&strokes[..1], &pins[..1], SURFACE), marks[..2]);
    }

    #[test]
    fn labels_sit_beside_what_they_number_and_stay_on_the_surface() {
        let label = |shape: Shape, bounds: [f32; 4]| {
            Mark {
                number: 1,
                shape,
                strokes: 0..1,
                span: Span::at(Moment::START),
                bounds: bounds.into(),
            }
            .label(1000.0, 500.0, 10.0)
        };
        let pin = label(
            Shape::Pin {
                point: Point::new(0.5, 0.5),
            },
            [0.5, 0.5, 0.5, 0.5],
        );
        assert!(pin.x > 505.0 && pin.y < 245.0, "{pin:?}");
        let arrow = label(
            Shape::Arrow {
                from: Point::new(0.2, 0.5),
                to: Point::new(0.6, 0.5),
            },
            [0.2, 0.5, 0.6, 0.5],
        );
        assert_eq!(arrow, Point::new(184.0, 250.0), "past the tail");
        let underline = label(Shape::Underline, [0.2, 0.5, 0.6, 0.52]);
        assert_eq!(underline, Point::new(616.0, 255.0), "past the right end");
        let circle = label(Shape::Circle, [0.2, 0.2, 0.6, 0.8]);
        assert!(circle.x > 600.0 && circle.y < 100.0, "{circle:?}");
        let cornered = label(
            Shape::Pin {
                point: Point::new(1.0, 0.0),
            },
            [1.0, 0.0, 1.0, 0.0],
        );
        assert_eq!(cornered, Point::new(989.0, 11.0), "kept on the surface");
    }

    #[test]
    fn underlines_report_their_extent_and_stroke_count() {
        let marks = group(
            &[
                stroke(&[[0.1, 0.5], [0.8, 0.5]], 0),
                stroke(&[[0.2, 0.52], [0.7, 0.52]], 200),
            ],
            &[],
            SURFACE,
        );
        assert_eq!(
            marks[0].to_string(),
            "① underlined x 0.10–0.80 at y 0.51 (2 strokes)"
        );
    }
}
