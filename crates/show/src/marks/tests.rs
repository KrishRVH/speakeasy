//! Grouping strokes and pins into numbered marks, reading their shapes, and placing their labels.

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
    let flat: Vec<[f32; 2]> = loop_points
        .iter()
        .map(|[x, y]| [x * 2.0 - 0.5, y * 0.4 + 0.3])
        .collect();
    assert_eq!(
        shape_of(&flat, half_height),
        Shape::Circle,
        "A flat loop around a wide control"
    );
    let zigzag = [
        [0.1, 0.40],
        [0.7, 0.42],
        [0.1, 0.44],
        [0.7, 0.46],
        [0.12, 0.40],
    ];
    assert_eq!(
        shape_of(&zigzag, half_height),
        Shape::Scribble,
        "A flat zigzag"
    );
    let tiny = Size {
        width: 20.0,
        height: 20.0,
    };
    assert_eq!(
        shape_of(&loop_points, tiny),
        Shape::Tap,
        "The same loop on a tiny surface"
    );
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
        "A pin between strokes drawn close together ends a mark"
    );
    assert_eq!(
        group(&strokes[..1], &pins[..1], SURFACE),
        marks[..2],
        "Later marks never renumber earlier ones, so live labels match the bundle's"
    );
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
        .label_center(1000.0, 500.0, 10.0)
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
    assert_eq!(arrow, Point::new(184.0, 250.0), "Past the tail");
    let underline = label(Shape::Underline, [0.2, 0.5, 0.6, 0.52]);
    assert_eq!(underline, Point::new(616.0, 255.0), "Past the right end");
    let circle = label(Shape::Circle, [0.2, 0.2, 0.6, 0.8]);
    assert!(circle.x > 600.0 && circle.y < 100.0, "{circle:?}");
    let cornered = label(
        Shape::Pin {
            point: Point::new(1.0, 0.0),
        },
        [1.0, 0.0, 1.0, 0.0],
    );
    assert_eq!(cornered, Point::new(989.0, 11.0), "Kept on the surface");
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
