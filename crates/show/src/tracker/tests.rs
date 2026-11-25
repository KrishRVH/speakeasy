use super::*;

const SURFACE: Size = Size {
    width: 1000.0,
    height: 500.0,
};

fn replay(samples: &[(u64, f32, f32, bool, bool)]) -> (Vec<Stroke>, Vec<Action>) {
    let mut tracker = Tracker::new(SURFACE);
    let mut end = Moment::START;
    for &(millis, x, y, drawing, pressed) in samples {
        end = Moment::from_millis(millis);
        tracker.observe(end, Point::new(x, y), drawing, pressed);
    }
    let recording = tracker.finish(end);
    (recording.strokes, recording.actions)
}

#[test]
fn shift_and_movement_draw_a_stroke() {
    let (strokes, actions) = replay(&[
        (0, 0.10, 0.10, true, false),
        (8, 0.12, 0.10, true, false),
        (16, 0.14, 0.11, true, false),
        (24, 0.14, 0.11, false, false),
    ]);
    assert_eq!(strokes.len(), 1);
    assert_eq!(strokes[0].points().len(), 3);
    assert_eq!(actions, Vec::<Action>::new());
}

#[test]
fn ink_shows_once_it_spans_enough_to_be_deliberate() {
    let mut tracker = Tracker::new(SURFACE);
    tracker.observe(Moment::START, Point::new(0.500, 0.5), true, false);
    tracker.observe(Moment::from_millis(8), Point::new(0.502, 0.5), true, false);
    assert!(tracker.drawing().is_empty(), "Shift alone shows no ink");
    tracker.observe(
        Moment::from_millis(16),
        Point::new(0.502, 0.5),
        false,
        false,
    );
    assert!(tracker.strokes().is_empty(), "Shift alone saves no ink");

    tracker.observe(Moment::from_millis(24), Point::new(0.502, 0.5), true, false);
    tracker.observe(Moment::from_millis(32), Point::new(0.510, 0.5), true, false);
    assert_eq!(tracker.drawing().len(), 2);
}

#[test]
fn a_stroke_cut_for_length_keeps_drawing_one_joined_line() {
    let mut tracker = Tracker::new(SURFACE);

    let draw = |tracker: &mut Tracker, index: u16| {
        let turn = f32::from(index) / 100.0;
        let point = Point::new(0.5 + 0.2 * turn.cos(), 0.5 + 0.4 * turn.sin());
        tracker.observe(Moment::from_millis(index.into()), point, true, false);
    };
    let cut = u16::try_from(MOST_STROKE_POINTS).unwrap();
    for index in 0..=cut {
        draw(&mut tracker, index);
    }
    let ended = tracker.strokes()[0].points().last().copied();
    assert_eq!(tracker.drawing().first().copied(), ended);
    draw(&mut tracker, cut + 1);
    let strokes = tracker.finish(Moment::from_millis(2_402)).strokes;
    assert_eq!(strokes.len(), 2);
    assert_eq!(strokes[0].points().last(), strokes[1].points().first());
}

#[derive(Clone, Copy)]
enum Tap {
    Clean,
    Flick,
    ShiftClick,
}

fn tap(tracker: &mut Tracker, millis: u64, x: f32, kind: Tap) -> Span {
    let at = |offset: u64| Moment::from_millis(millis.saturating_add(offset));
    let (flicked, pressed) = match kind {
        Tap::Clean => (x, false),
        Tap::Flick => (x + 0.02, false),
        Tap::ShiftClick => (x, true),
    };
    tracker.observe(at(0), Point::new(x, 0.4), true, pressed);
    tracker.observe(at(30), Point::new(flicked, 0.4), true, pressed);
    tracker.observe(at(60), Point::new(flicked, 0.4), false, false);
    Span::new(at(0), at(60))
}

fn clean(tracker: &mut Tracker, millis: u64, x: f32) -> Span {
    tap(tracker, millis, x, Tap::Clean)
}

fn pin(tracker: &mut Tracker, first: Span, second: Span) -> bool {
    tracker.pin(Span::new(first.start(), second.end()))
}

#[test]
fn accepting_a_pin_retires_only_ink_begun_during_its_taps() {
    let at = Moment::from_millis;
    for (inked, delivered_first, start) in [(40, false, 70), (60, false, 60), (45, true, 70)] {
        let mut tracker = Tracker::new(SURFACE);
        tracker.observe(at(5), Point::new(0.5, 0.5), false, false);
        let ink = |tracker: &mut Tracker| {
            tracker.observe(at(inked), Point::new(0.5, 0.5), true, false);
        };
        if !delivered_first {
            ink(&mut tracker);
        }
        assert!(tracker.pin(Span::new(at(10), at(50))));
        if delivered_first {
            ink(&mut tracker);
        }
        tracker.observe(at(70), Point::new(0.508, 0.5), true, false);
        tracker.observe(at(80), Point::new(0.55, 0.5), true, false);
        let recording = tracker.finish(at(90));
        assert_eq!(
            recording.strokes[0].span().start(),
            at(start),
            "Ink seen at {inked} ms, pin delivered first: {delivered_first}"
        );
    }
}

#[test]
fn a_tap_that_draws_after_another_shift_hold_between_polls_drops_no_pin() {
    let mut tracker = Tracker::new(SURFACE);
    let at = Moment::from_millis;
    tracker.observe(at(0), Point::new(0.5, 0.4), true, false);
    tracker.observe(at(30), Point::new(0.5, 0.4), true, false);
    tracker.observe(at(60), Point::new(0.52, 0.4), true, false);
    tracker.observe(at(90), Point::new(0.52, 0.4), false, false);
    tracker.observe(at(150), Point::new(0.52, 0.4), true, false);
    tracker.observe(at(210), Point::new(0.52, 0.4), false, false);
    assert!(
        !tracker.pin(Span::new(at(40), at(200))),
        "A first tap that drew ink dropped a pin because Shift stayed down across polls"
    );
}

#[test]
fn pins_drop_where_the_pointer_was_last_seen_on_the_surface_after_two_clean_taps() {
    let mut tracker = Tracker::new(SURFACE);
    let t = &mut tracker;
    assert!(
        !t.pin(Span::at(Moment::START)),
        "The pointer hasn't been seen"
    );
    let (first, second) = (clean(t, 0, 0.3), clean(t, 160, 0.3));
    assert!(pin(t, first, second));
    let third = clean(t, 320, 0.3);
    assert!(!pin(t, second, third), "Three taps pin once");
    let (first, second) = (clean(t, 1_000, 1.3), clean(t, 1_160, 1.3));
    assert!(!pin(t, first, second), "On another display");
    let (first, second) = (tap(t, 2_000, 0.5, Tap::ShiftClick), clean(t, 2_160, 0.5));
    assert!(!pin(t, first, second), "Shift-clicking");
    let (first, second) = (
        tap(t, 3_000, 0.5, Tap::Flick),
        tap(t, 3_160, 0.5, Tap::Flick),
    );
    assert!(!pin(t, first, second), "Two quick strokes, as in an X");
    let stroke = tap(t, 4_000, 0.6, Tap::Flick);
    let (first, second) = (clean(t, 4_160, 0.62), clean(t, 4_320, 0.62));
    assert!(!pin(t, stroke, first), "A quick stroke and a tap");
    assert!(
        pin(t, first, second),
        "A quick stroke costs the double tap after it nothing"
    );
    let (first, second) = (clean(t, 5_000, 0.7), clean(t, 5_160, 0.7));
    tap(t, 5_240, 0.7, Tap::Flick);
    assert!(
        pin(t, first, second),
        "A stroke seen after the taps but before their pin"
    );
    let (first, second) = (clean(t, 5_500, 0.75), clean(t, 5_660, 0.75));
    t.observe(
        Moment::from_millis(5_740),
        Point::new(1.3, 0.4),
        false,
        false,
    );
    assert!(
        pin(t, first, second),
        "The pointer left the surface after the taps but before their pin"
    );
    let recording = tracker.finish(Moment::from_millis(6_000));
    assert_eq!(
        recording.pins,
        [
            Pin {
                at: Moment::from_millis(220),
                point: Point::new(0.3, 0.4)
            },
            Pin {
                at: Moment::from_millis(4_380),
                point: Point::new(0.62, 0.4)
            },
            Pin {
                at: Moment::from_millis(5_220),
                point: Point::new(0.7, 0.4)
            },
            Pin {
                at: Moment::from_millis(5_720),
                point: Point::new(0.75, 0.4)
            }
        ]
    );
    assert_eq!(recording.strokes.len(), 4, "The flicks drew");
    assert_eq!(recording.duration, Moment::from_millis(6_000));
}

#[test]
fn presses_become_clicks_or_drags_by_how_far_they_moved() {
    let (_, actions) = replay(&[
        (0, 0.50, 0.50, false, true),
        (8, 0.505, 0.50, false, true),
        (100, 0.505, 0.50, false, false),
        (200, 0.20, 0.20, false, true),
        (300, 0.40, 0.20, false, true),
        (400, 0.40, 0.20, false, false),
    ]);
    assert_eq!(
        actions,
        [
            Action::Click {
                at: Moment::from_millis(100),
                point: Point::new(0.50, 0.50)
            },
            Action::Drag {
                span: Span::new(Moment::from_millis(200), Moment::from_millis(400)),
                from: Point::new(0.20, 0.20),
                to: Point::new(0.40, 0.20)
            }
        ]
    );
}

#[test]
fn drawing_never_clicks_and_other_displays_are_ignored() {
    let (strokes, actions) = replay(&[
        (0, 0.30, 0.30, true, true),
        (8, 0.35, 0.30, true, true),
        (16, 0.35, 0.30, false, false),
        (100, 1.40, 0.30, false, true),
        (108, 1.40, 0.30, false, false),
        (200, 1.20, 0.50, true, false),
        (208, 1.30, 0.50, true, false),
    ]);
    assert_eq!(strokes.len(), 1);
    assert_eq!(actions, Vec::<Action>::new());
}

#[test]
fn a_press_held_at_the_end_still_counts_as_what_it_was() {
    let (_, actions) = replay(&[(0, 0.5, 0.5, false, true), (50, 0.5, 0.5, false, true)]);
    assert_eq!(
        actions,
        [Action::Click {
            at: Moment::from_millis(50),
            point: Point::new(0.5, 0.5)
        }]
    );
    let (_, actions) = replay(&[(0, 0.2, 0.2, false, true), (50, 0.4, 0.2, false, true)]);
    assert_eq!(
        actions,
        [Action::Drag {
            span: Span::new(Moment::START, Moment::from_millis(50)),
            from: Point::new(0.2, 0.2),
            to: Point::new(0.4, 0.2)
        }]
    );
}
