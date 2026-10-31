use super::*;

#[test]
fn a_future_cut_segment_cannot_repaint_the_carried_joint() {
    let prefix = recording(vec![stroke(&[(0.1, 0.5, 100), (0.8, 0.5, 200)])]);
    let mut continued = prefix.clone();
    continued
        .strokes
        .push(stroke(&[(0.8, 0.5, 200), (0.9, 0.8, 300)]));
    continued.spaces.strokes.push(continued.spaces.strokes[0]);
    let before = Annotations::new(&prefix, &[]).unwrap();
    let after = Annotations::new(&continued, &[]).unwrap();
    for at in [200, 250] {
        assert_eq!(
            painted(&before, Duration::from_millis(at)),
            painted(&after, Duration::from_millis(at))
        );
    }
}

#[test]
fn a_mark_cannot_declare_completion_before_its_referenced_strokes() {
    let mut tracker = Tracker::new(Size {
        width: 1000.0,
        height: 1000.0,
    });
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(100), Point::new(0.2, 0.5), true, false);
    tracker.observe(ms(200), Point::new(0.8, 0.5), true, false);
    tracker.observe(ms(210), Point::new(0.8, 0.5), false, false);
    let mut marks = tracker.marks();
    let recording = tracker.finish(ms(1000));
    assert!(Annotations::new(&recording, &marks).is_ok());
    marks[0].span = Span::at(ms(100));
    assert!(Annotations::new(&recording, &marks).is_err());
}

#[test]
fn cancellation_between_primitives_leaves_the_original_frame_untouched() {
    let recording = recording(vec![
        stroke(&[(0.1, 0.3, 100), (0.8, 0.3, 200)]),
        stroke(&[(0.1, 0.7, 300), (0.8, 0.7, 400)]),
    ]);
    let scene = Annotations::new(&recording, &[]).unwrap();
    let mut image = blank();
    #[expect(
        clippy::disallowed_types,
        reason = "The synchronous callback needs an interior counter to cancel a later batch"
    )]
    let polls = std::cell::Cell::new(0);
    let result = scene
        .paint(
            &mut image,
            Duration::from_millis(500),
            VIEW,
            &typeface(),
            || {
                polls.set(polls.get() + 1);
                polls.get() >= 3
            },
        )
        .unwrap();
    assert!(result.is_none());
    assert_eq!(image, blank());
    assert_eq!(polls.get(), 3);
}

#[test]
fn ambiguous_pin_identity_cannot_acquire_a_timing_proof() {
    let mut tracker = Tracker::new(Size {
        width: 1000.0,
        height: 1000.0,
    });
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(90), Point::new(0.0, 0.5), false, false);
    assert!(tracker.pin(Span::new(ms(80), ms(100))));
    let marks = tracker.marks();
    let mut recording = tracker.finish(ms(1000));
    let scene = Annotations::new(&recording, &marks).unwrap();
    assert_eq!(painted(&scene, Duration::from_millis(100)).1.drawn, 1);
    let mut duplicate = recording.pins[0];
    duplicate.point.x = -0.0;
    recording.pins.push(duplicate);
    recording.spaces.pins.push(recording.spaces.pins[0]);
    let scene = Annotations::new(&recording, &marks).unwrap();
    assert_eq!(
        painted(&scene, Duration::from_millis(100)).1,
        AnnotationReport {
            drawn: 0,
            omitted: 1
        }
    );
}
