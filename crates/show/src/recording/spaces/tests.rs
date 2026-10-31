use super::*;
use crate::{Point, Size, Span, Tracker};

const WHOLE: Rect = Rect {
    left: 0.0,
    top: 0.0,
    right: 1.0,
    bottom: 1.0,
};
const RESIZED: Rect = Rect {
    left: 0.25,
    top: 0.0,
    right: 0.75,
    bottom: 1.0,
};

fn ms(value: u64) -> crate::Moment {
    crate::Moment::from_millis(value)
}

fn tracker() -> Tracker {
    Tracker::new(Size {
        width: 1000.0,
        height: 1000.0,
    })
}

#[test]
fn delayed_pins_follow_the_point_used_instead_of_the_tap_timestamp() {
    let mut tracker = tracker();
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(50), Point::new(0.2, 0.3), false, false);
    assert!(tracker.locate(Some(RESIZED)));
    tracker.observe(ms(120), Point::new(0.6, 0.7), false, false);
    assert!(tracker.pin(Span::new(ms(80), ms(100))));
    let mark = tracker.marks().remove(0);
    let recording = tracker.finish(ms(150));
    assert_eq!(recording.mark_part(&mark), Some(RESIZED));
    assert_eq!(recording.pins[0].point, Point::new(0.6, 0.7));
    assert_eq!(recording.spaces.pins[0].unwrap().observed, Some(ms(120)));
}

#[test]
fn legacy_pin_proofs_keep_geometry_without_inventing_timing_or_action_proofs() {
    let mut spaces: Spaces = serde_json::from_value(serde_json::json!({
        "strokes":[], "pins":[{"generation":1,"part":[0,0,1,1]}]
    }))
    .unwrap();
    assert!(spaces.valid(0, 1, 3, ms(100)));
    assert_eq!(spaces.pins[0].unwrap().observed, None);
    spaces.pins[0].as_mut().unwrap().observed = Some(ms(101));
    assert!(!spaces.valid(0, 1, 3, ms(100)));
    spaces.pins[0].as_mut().unwrap().observed = Some(ms(100));
    spaces.actions = vec![None];
    assert!(!spaces.valid(0, 1, 3, ms(100)));
}

#[test]
fn actions_include_the_release_space_but_finalization_uses_the_stored_endpoint() {
    for release in [true, false] {
        let mut tracker = tracker();
        assert!(tracker.locate(Some(WHOLE)));
        tracker.observe(ms(10), Point::new(0.2, 0.3), false, true);
        assert!(tracker.locate(Some(RESIZED)));
        if release {
            tracker.observe(ms(20), Point::new(0.6, 0.7), false, false);
        }
        let recording = tracker.finish(ms(30));
        assert_eq!(recording.actions.len(), 1);
        assert_eq!(
            recording.spaces.actions[0].map(|space| space.part),
            (!release).then_some(WHOLE)
        );
    }
    let mut tracker = tracker();
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(10), Point::new(0.2, 0.3), false, true);
    assert!(tracker.locate(Some(RESIZED)));
    tracker.observe(ms(20), Point::new(0.4, 0.3), false, true);
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(30), Point::new(0.6, 0.3), false, false);
    assert_eq!(tracker.finish(ms(40)).spaces.actions, [None]);
}

#[test]
fn grouped_marks_cannot_cross_resizes_gaps_or_unknown_legacy_samples() {
    for mode in 0..4 {
        let mut tracker = tracker();
        assert!(tracker.locate(Some(WHOLE)));
        tracker.observe(ms(0), Point::new(0.1, 0.2), true, false);
        tracker.observe(ms(10), Point::new(0.5, 0.2), true, false);
        tracker.observe(ms(20), Point::new(0.5, 0.2), false, false);
        match mode {
            0 => {
                assert!(tracker.locate(Some(RESIZED)));
            },
            1 => {
                assert!(tracker.locate(Some(RESIZED)));
                assert!(tracker.locate(Some(WHOLE)));
            },
            2 => {
                tracker.gap(ms(25));
                assert!(tracker.locate(Some(WHOLE)));
            },
            _ => {
                assert!(tracker.locate(None));
            },
        }
        tracker.observe(ms(30), Point::new(0.1, 0.4), true, false);
        tracker.observe(ms(40), Point::new(0.5, 0.4), true, false);
        tracker.observe(ms(50), Point::new(0.5, 0.4), false, false);
        let marks = tracker.marks();
        assert_eq!(marks.len(), 1);
        let recording = tracker.finish(ms(60));
        assert_eq!(recording.mark_part(&marks[0]), None, "mode {mode}");
    }
}

#[test]
fn a_cut_preserves_the_carried_points_space_before_folding_the_new_point() {
    let mut tracker = tracker();
    assert!(tracker.locate(Some(WHOLE)));
    for index in 0..4800 {
        if index == 2400 {
            assert!(tracker.locate(Some(RESIZED)));
        }
        tracker.observe(
            ms(index),
            Point::new(if index % 2 == 0 { 0.1 } else { 0.9 }, 0.5),
            true,
            false,
        );
    }
    let recording = tracker.finish(ms(4801));
    assert_eq!(recording.strokes.len(), 3);
    let parts: Vec<_> = recording
        .spaces
        .strokes
        .iter()
        .map(|space| space.map(|space| space.part))
        .collect();
    assert_eq!(parts, [Some(WHOLE), None, Some(RESIZED)]);
}

#[test]
fn mixed_strokes_and_unbound_proof_vectors_never_supply_crop_coordinates()
-> Result<(), serde_json::Error> {
    let mut tracker = tracker();
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(0), Point::new(0.1, 0.2), true, false);
    tracker.observe(ms(10), Point::new(0.3, 0.2), true, false);
    assert!(tracker.locate(Some(RESIZED)));
    tracker.observe(ms(20), Point::new(0.6, 0.2), true, false);
    tracker.observe(ms(30), Point::new(0.6, 0.2), false, false);
    let mark = tracker.marks().remove(0);
    let mut recording = tracker.finish(ms(40));
    assert_eq!(recording.mark_part(&mark), None);
    recording.spaces.strokes.push(None);
    assert!(!recording.spaces.valid(
        recording.strokes.len(),
        recording.pins.len(),
        recording.actions.len(),
        recording.duration
    ));
    assert_eq!(recording.mark_part(&mark), None);
    let bad: Spaces = serde_json::from_value(
        serde_json::json!({"strokes":[{"generation":1,"part":[0,0,2,1]}],"pins":[]}),
    )?;
    assert!(!bad.valid(1, 0, 0, ms(40)));
    Ok(())
}
