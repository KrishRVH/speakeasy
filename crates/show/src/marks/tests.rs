//! Grouping strokes and pins into numbered marks, reading their shapes from generated strokes and
//! strokes of real captures, and placing their labels.

use std::{f32::consts::TAU, iter};

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

/// Corners joined by points 4 px apart on [`SURFACE`], as a mouse samples them.
fn polyline(corners: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let corners: Vec<Point> = corners.iter().copied().map(Point::from).collect();
    corners
        .array_windows()
        .flat_map(|&[from, to]| {
            let step = 0.004 / from.distance(to);
            iter::successors(Some(0.0), move |done| Some(done + step))
                .take_while(|&done| done < 1.0)
                .map(move |done| from.lerp(to, done))
        })
        .chain(corners.last().copied())
        .map(Into::into)
        .collect()
}

/// Points 3° apart round an ellipse, from angle `from` to `to` in radians.
fn ellipse(center: [f32; 2], radii: [f32; 2], from: f32, to: f32) -> Vec<[f32; 2]> {
    iter::successors(Some(from), |angle| Some(angle + TAU / 120.0))
        .take_while(|&angle| angle < to)
        .chain([to])
        .map(|angle| {
            [
                center[0] + radii[0] * angle.cos(),
                center[1] + radii[1] * angle.sin(),
            ]
        })
        .collect()
}

/// `samples` points back and forth over a line of text, drifting up and down.
fn scribble(samples: u16) -> Vec<[f32; 2]> {
    (0..samples)
        .map(f32::from)
        .map(|step| {
            [
                0.2 + 0.1 * (0.9 * step).sin(),
                0.2 + 0.022 * (0.37 * step).sin(),
            ]
        })
        .collect()
}

/// A stroke of a real capture, from `x, y, ms` triples begun `at` milliseconds into the Show.
fn real(triples: &[u16], at: u64) -> Stroke {
    Stroke::new(
        triples
            .as_chunks()
            .0
            .iter()
            .map(|&[x, y, ms]| InkPoint {
                point: scrawl(x, y),
                at: Moment::from_millis(at.saturating_add(u64::from(ms))),
            })
            .collect(),
    )
    .unwrap()
}

/// A point Scrawl stores in ten-thousandths of the surface.
fn scrawl(x: u16, y: u16) -> Point {
    Point::new(f32::from(x) / 10_000.0, f32::from(y) / 10_000.0)
}

/// The corners of an arrow drawn in one go: its shaft, then a barb out, back to the tip, and a
/// barb out on the other side.
const TWO_BARBED_ARROW: [[f32; 2]; 5] = [
    [0.1, 0.3],
    [0.3, 0.25],
    [0.26, 0.23],
    [0.3, 0.25],
    [0.27, 0.27],
];

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

/// Strokes drawn with a mouse in real captures: a flat loop round a table cell, an arrow drawn in
/// one go, and underlines whose ends hook back like a lone barb.
#[test]
fn real_strokes_read_as_they_were_drawn() {
    let marks = group(
        &[real(&REAL_LOOP, 6_208), real(&REAL_ARROW, 10_283)],
        &[],
        CAPTURE,
    );
    let described: Vec<String> = marks.iter().map(ToString::to_string).collect();
    assert_eq!(
        described,
        [
            "① circled region x 0.30–0.41, y 0.25–0.32",
            "② drew an arrow from x 0.41, y 0.30 pointing to x 0.47, y 0.31"
        ]
    );
    assert_eq!(
        marks[1].shape,
        Shape::Arrow {
            from: scrawl(4057, 2996),
            to: scrawl(4653, 3131)
        },
        "To the tip, not to where the pen lifted after the lower barb"
    );
    for hook in REAL_HOOKED_UNDERLINES {
        let shape = group(&[real(hook, 0)], &[], HOOK_CAPTURE)[0].shape;
        assert!(
            !matches!(shape, Shape::Arrow { .. }),
            "A hooked underline became {shape:?}"
        );
    }
}

#[test]
fn an_arrow_drawn_in_one_stroke_points_at_its_tip() {
    let arrows: [(&str, &[[f32; 2]]); 3] = [
        (
            "one barb across the shaft",
            &[[0.1, 0.3], [0.3, 0.25], [0.262, 0.236], [0.272, 0.275]],
        ),
        ("two barbs", &TWO_BARBED_ARROW),
        (
            "a small head on a long shaft",
            &[
                [0.1, 0.5],
                [0.4, 0.5],
                [0.38, 0.488],
                [0.4, 0.5],
                [0.38, 0.512],
            ],
        ),
    ];
    for (name, corners) in arrows {
        let shape = shape_of(&polyline(corners), SURFACE);
        let Shape::Arrow { from, to } = shape else {
            panic!("{name} became {shape:?}");
        };
        assert_eq!(from, corners[0].into(), "{name}");
        assert!(to.distance(corners[1].into()) < 0.001, "{name}: {to:?}");
    }
}

#[test]
fn arrows_drawn_one_after_another_are_not_one_arrow() {
    let arrow = polyline(&TWO_BARBED_ARROW);
    let lower: Vec<[f32; 2]> = arrow.iter().map(|&[x, y]| [x, y + 0.2]).collect();
    let marks = group(&[stroke(&arrow, 0), stroke(&lower, 500)], &[], SURFACE);
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].shape, Shape::Scribble);
}

/// A loop that runs on into a tail or past where it began, or starts with a lead-in, still
/// circles something.
#[test]
fn a_loop_with_a_lead_in_a_tail_or_an_overshoot_is_a_circle() {
    let flat = |from: f32, to: f32| ellipse([0.3, 0.3], [0.1, 0.05], from, to);
    let cases = [
        (
            "tail",
            [flat(0.0, TAU), polyline(&[[0.4, 0.3], [0.52, 0.38]])].concat(),
        ),
        ("overshoot", flat(0.0, 1.3 * TAU)),
        (
            "lead-in",
            [
                polyline(&[[0.18, 0.2], [0.3, 0.25]]),
                flat(-0.25 * TAU, 0.8 * TAU),
            ]
            .concat(),
        ),
    ];
    for (name, points) in cases {
        assert_eq!(shape_of(&points, SURFACE), Shape::Circle, "{name}");
    }
    let tail = [
        3450, 3260, 1190, 3380, 3330, 1200, 3300, 3420, 1210, 3220, 3500, 1220,
    ];
    let tailed = real(&[&REAL_LOOP[..], &tail].concat(), 0);
    assert_eq!(
        group(&[tailed], &[], CAPTURE)[0].shape,
        Shape::Circle,
        "A real loop with a tail"
    );
}

/// The flat-loop guards and the search inside strokes lose no loop that closes at its ends: round
/// and flat loops, loops open on either axis, and loops gone round again.
#[test]
fn every_loop_closed_at_its_ends_is_still_a_circle() {
    let center = [0.3, 0.3];
    let cases = [
        ellipse(center, [0.1, 0.1], 0.0, TAU),
        ellipse(center, [0.1, 0.05], 0.0, TAU),
        // Open by about 30° at one end of its long axis.
        ellipse(center, [0.115, 0.05], 0.25 * TAU, 1.25 * TAU - 0.55),
        ellipse(center, [0.1, 0.1], 0.3, TAU - 0.3),
        ellipse(center, [0.05, 0.1], 0.2, TAU - 0.2),
        ellipse(center, [0.1, 0.08], 0.0, 2.0 * TAU),
        ellipse(center, [0.1, 0.1], 0.0, 3.0 * TAU),
        ellipse(center, [0.1, 0.1], 0.0, 5.0 * TAU),
        // Around one line of text, 6:1: closed, gone round twice, and open by 15 px.
        ellipse(center, [0.15, 0.025], 0.0, TAU),
        ellipse(center, [0.15, 0.025], 0.0, 2.0 * TAU),
        ellipse(center, [0.15, 0.025], 0.1, TAU),
    ];
    for (case, points) in cases.iter().enumerate() {
        assert_eq!(shape_of(points, SURFACE), Shape::Circle, "case {case}");
    }
}

/// Going round and round a line of text is scribbling over it.
#[test]
fn a_flat_loop_counts_up_to_three_turns() {
    for turns in [1.0, 3.0] {
        let points = ellipse([0.3, 0.3], [0.12, 0.03], 0.0, turns * TAU);
        assert_eq!(shape_of(&points, SURFACE), Shape::Circle, "{turns} turns");
    }
    let points = ellipse([0.3, 0.3], [0.12, 0.03], 0.0, 4.0 * TAU);
    assert_eq!(shape_of(&points, SURFACE), Shape::Scribble);
}

/// The loop search tries only every few points of a long stroke as a loop's ends, but judges each
/// stretch by every point along it, so sampling neither smooths a zigzag away nor folds many
/// turns into one.
#[test]
fn sampling_a_long_stroke_hides_neither_zigzags_nor_turns() {
    // Every third point is on a smooth flat ellipse, and the others zigzag across it.
    let mut zigzag: Vec<[f32; 2]> = (0..400_u16)
        .flat_map(|step| {
            let angle = TAU * f32::from(step) / 400.0;
            let [x, y] = [0.3 + 0.12 * angle.cos(), 0.3 + 0.03 * angle.sin()];
            [[x, y], [x + 0.004, y + 0.008], [x - 0.004, y - 0.008]]
        })
        .collect();
    zigzag.push(zigzag[0]);
    // 479 turns in 2,395 steps, within what the tracker keeps of a stroke: every fifth point, each
    // one tried as an end, is a whole turn on.
    let turns: Vec<[f32; 2]> = (0..=2_395_u16)
        .map(|step| {
            let angle = TAU * f32::from(step) / 5.0;
            [0.3 + 0.12 * angle.cos(), 0.3 + 0.03 * angle.sin()]
        })
        .collect();
    for (name, points) in [("zigzag", zigzag), ("479 turns", turns)] {
        assert_eq!(shape_of(&points, SURFACE), Shape::Scribble, "{name}");
    }
}

#[test]
fn ticks_zigzags_scribbles_hooks_and_arcs_are_neither_arrows_nor_circles() {
    let cases = [
        (
            "check mark",
            polyline(&[[0.1, 0.2], [0.13, 0.24], [0.22, 0.12]]),
        ),
        ("V", polyline(&[[0.1, 0.1], [0.15, 0.25], [0.2, 0.1]])),
        (
            "narrow V",
            polyline(&[[0.1, 0.1], [0.13, 0.25], [0.16, 0.1]]),
        ),
        (
            "zigzag",
            polyline(&[
                [0.1, 0.2],
                [0.13, 0.16],
                [0.16, 0.2],
                [0.19, 0.16],
                [0.22, 0.2],
            ]),
        ),
        (
            "back and forth over a line of text",
            polyline(&[
                [0.1, 0.2],
                [0.3, 0.21],
                [0.11, 0.225],
                [0.3, 0.24],
                [0.11, 0.255],
            ]),
        ),
        ("brief scribble", scribble(60)),
        // Closes on itself and fills its box: only its length tells it from a circle.
        ("ten-second scribble", scribble(600)),
        (
            "hooked underline",
            polyline(&[[0.1, 0.3], [0.4, 0.3], [0.405, 0.27]]),
        ),
        (
            "curled underline",
            polyline(&[
                [0.1, 0.3],
                [0.4, 0.3],
                [0.415, 0.29],
                [0.415, 0.275],
                [0.4, 0.265],
            ]),
        ),
        (
            "one-sided barb",
            polyline(&[[0.1, 0.3], [0.3, 0.25], [0.26, 0.24]]),
        ),
        ("line", polyline(&[[0.1, 0.3], [0.4, 0.3]])),
        (
            "C",
            ellipse([0.3, 0.3], [0.1, 0.1], 0.125 * TAU, 0.875 * TAU),
        ),
        ("flat C", ellipse([0.3, 0.3], [0.1, 0.025], 0.8, TAU - 0.8)),
        (
            "hourglass",
            polyline(&[[0.1, 0.1], [0.3, 0.1], [0.1, 0.3], [0.3, 0.3], [0.1, 0.1]]),
        ),
    ];
    for (name, points) in cases {
        let shape = shape_of(&points, SURFACE);
        assert!(
            !matches!(shape, Shape::Arrow { .. } | Shape::Circle),
            "{name} became {shape:?}"
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

/// Two strokes of one real Scrawl capture, drawn with a mouse on a 1728 × 1117 screen: a flat loop
/// round a table cell that ends just past where it began, and an arrow drawn in one go, its shaft
/// left to right, then a barb back above it, back to the tip, and a barb below. Strokes are
/// `x, y, ms` triples: ten-thousandths of the surface, and milliseconds since the pen went down.
/// Scrawl's test data, under the MIT License; see NOTICE.md.
const CAPTURE: Size = Size {
    width: 1728.0,
    height: 1117.0,
};
const REAL_LOOP: [u16; 207] = [
    3617, 3170, 0, 3611, 3184, 48, 3595, 3194, 76, 3582, 3200, 96, 3560, 3210, 106, 3529, 3217,
    126, 3495, 3225, 146, 3449, 3229, 156, 3399, 3229, 175, 3363, 3229, 195, 3321, 3229, 215, 3276,
    3229, 225, 3249, 3229, 245, 3214, 3225, 263, 3192, 3218, 281, 3153, 3207, 290, 3136, 3199, 310,
    3093, 3172, 330, 3069, 3152, 350, 3051, 3132, 370, 3040, 3117, 388, 3033, 3103, 397, 3026,
    3084, 417, 3024, 3051, 437, 3024, 3030, 447, 3024, 3009, 457, 3024, 2971, 467, 3024, 2928, 487,
    3026, 2890, 505, 3041, 2841, 515, 3073, 2779, 544, 3139, 2696, 562, 3197, 2640, 581, 3228,
    2616, 591, 3330, 2563, 611, 3398, 2544, 631, 3477, 2533, 641, 3550, 2529, 661, 3623, 2529, 681,
    3702, 2539, 691, 3780, 2579, 710, 3852, 2634, 730, 3913, 2676, 747, 3945, 2698, 757, 3983,
    2729, 776, 4011, 2746, 785, 4035, 2767, 804, 4055, 2790, 823, 4078, 2823, 834, 4088, 2848, 853,
    4099, 2885, 873, 4103, 2918, 883, 4103, 2990, 920, 4081, 3027, 938, 4045, 3067, 948, 4002,
    3105, 968, 3921, 3159, 987, 3900, 3171, 997, 3849, 3192, 1017, 3803, 3205, 1027, 3762, 3217,
    1052, 3720, 3225, 1062, 3678, 3225, 1080, 3636, 3225, 1090, 3594, 3225, 1110, 3566, 3225, 1128,
    3535, 3225, 1148, 3516, 3222, 1158, 3502, 3218, 1178,
];
const REAL_ARROW: [u16; 120] = [
    4057, 2996, 0, 4047, 2996, 68, 4075, 2996, 199, 4130, 3004, 209, 4236, 3044, 229, 4312, 3073,
    248, 4406, 3106, 256, 4490, 3121, 276, 4567, 3131, 296, 4606, 3131, 316, 4633, 3131, 326, 4653,
    3131, 365, 4621, 3111, 431, 4570, 3090, 451, 4507, 3062, 469, 4404, 3018, 488, 4373, 3007, 506,
    4345, 3000, 516, 4331, 3000, 534, 4322, 2997, 573, 4337, 2995, 624, 4376, 2995, 644, 4436,
    2999, 664, 4509, 3023, 674, 4549, 3039, 684, 4607, 3061, 694, 4638, 3076, 713, 4652, 3083, 723,
    4663, 3086, 733, 4674, 3092, 743, 4662, 3107, 830, 4648, 3115, 840, 4609, 3147, 850, 4530,
    3217, 880, 4512, 3233, 889, 4494, 3249, 899, 4483, 3259, 907, 4467, 3275, 917, 4458, 3283, 934,
    4481, 3284, 1308,
];

/// Underlines from real Scrawl captures on a 2560 × 1440 screen whose ends hook back like a lone
/// barb, which a rule that took one barb for an arrowhead called arrows.
const HOOK_CAPTURE: Size = Size {
    width: 2560.0,
    height: 1440.0,
};
const REAL_HOOKED_UNDERLINES: [&[u16]; 2] = [
    &[
        3589, 6331, 0, 3612, 6331, 30, 3665, 6347, 49, 3718, 6356, 68, 3787, 6365, 78, 3817, 6365,
        86, 3856, 6365, 95, 3871, 6365, 105, 3963, 6365, 115, 4001, 6365, 134, 4062, 6365, 143,
        4139, 6365, 153, 4169, 6365, 172, 4214, 6365, 182, 4244, 6377, 199, 4258, 6377, 218, 4272,
        6381, 227, 4302, 6381, 247, 4355, 6390, 266, 4392, 6405, 276, 4407, 6405, 286, 4422, 6411,
        295, 4429, 6415, 305, 4439, 6422, 314, 4445, 6431, 334, 4450, 6442, 353, 4453, 6451, 371,
        4455, 6465, 401, 4450, 6476, 411, 4442, 6491, 430, 4436, 6500, 449, 4426, 6510, 458, 4415,
        6519, 477, 4408, 6528, 487, 4394, 6544, 497, 4372, 6560, 506, 4357, 6567, 516, 4342, 6573,
        524, 4305, 6573, 538, 4274, 6573, 548, 4267, 6573, 558, 4244, 6565, 567, 4223, 6559, 576,
    ],
    &[
        7252, 494, 0, 7273, 484, 18, 7288, 477, 28, 7319, 468, 37, 7341, 468, 46, 7356, 461, 56,
        7371, 461, 64, 7409, 461, 81, 7477, 461, 91, 7508, 461, 99, 7530, 469, 109, 7545, 476, 119,
        7555, 476, 129, 7559, 485, 165, 7547, 504, 185, 7536, 524, 195, 7528, 532, 224, 7514, 541,
        234, 7460, 567, 254, 7407, 601, 264,
    ],
];
