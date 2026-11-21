use super::*;
use crate::{Point, Size, Span, Tracker};

const RESIZED: Rect = Rect {
    left: 0.25,
    top: 0.0,
    right: 0.75,
    bottom: 1.0,
};

fn ms(value: u64) -> Moment {
    Moment::from_millis(value)
}

fn tracker() -> Tracker {
    let mut tracker = Tracker::new(Size {
        width: 1000.0,
        height: 1000.0,
    });
    assert!(tracker.locate(Rect::UNIT));
    tracker
}

#[test]
fn delayed_pins_take_the_point_and_part_seen_when_their_taps_ended() {
    let mut tracker = tracker();
    tracker.observe(ms(50), Point::new(0.2, 0.3), false, false);
    assert!(tracker.locate(RESIZED));
    tracker.observe(ms(120), Point::new(0.6, 0.7), false, false);
    assert!(tracker.pin(Span::new(ms(80), ms(100))));
    let mark = tracker.marks().remove(0);
    let recording = tracker.finish(ms(150));
    assert_eq!(
        recording.mark_part(&mark),
        Some(Rect::UNIT),
        "A delayed pin took the part seen after its taps"
    );
    assert_eq!(recording.pins[0].point, Point::new(0.2, 0.3));
    assert_eq!(recording.spaces.pins[0].unwrap().observed, ms(50));
}

#[test]
fn pin_proofs_require_observation_and_complete_lane_vectors() {
    let mut saved = serde_json::json!({
        "strokes":[], "pins":[{"generation":1,"part":[0,0,1,1]}],
        "actions":[null,null,null]
    });
    assert!(serde_json::from_value::<Spaces>(saved.clone()).is_err());
    saved["pins"][0]["observed"] = serde_json::json!(100);
    let mut spaces: Spaces = serde_json::from_value(saved).unwrap();
    assert!(spaces.valid(0, 1, 3, ms(100)));
    spaces.pins[0].as_mut().unwrap().observed = ms(101);
    assert!(!spaces.valid(0, 1, 3, ms(100)));
    spaces.pins[0].as_mut().unwrap().observed = ms(100);
    spaces.actions = vec![None];
    assert!(!spaces.valid(0, 1, 3, ms(100)));
    spaces.actions.clear();
    assert!(!spaces.valid(0, 1, 3, ms(100)));
}

#[test]
fn actions_include_the_release_space_but_finalization_uses_the_stored_endpoint() {
    for release in [true, false] {
        let mut tracker = tracker();
        tracker.observe(ms(10), Point::new(0.2, 0.3), false, true);
        assert!(tracker.locate(RESIZED));
        if release {
            tracker.observe(ms(20), Point::new(0.6, 0.7), false, false);
        }
        let recording = tracker.finish(ms(30));
        assert_eq!(recording.actions.len(), 1);
        assert_eq!(
            recording.spaces.actions[0].map(|space| space.part),
            (!release).then_some(Rect::UNIT)
        );
    }
    let mut tracker = tracker();
    tracker.observe(ms(10), Point::new(0.2, 0.3), false, true);
    assert!(tracker.locate(RESIZED));
    tracker.observe(ms(20), Point::new(0.4, 0.3), false, true);
    assert!(tracker.locate(Rect::UNIT));
    tracker.observe(ms(30), Point::new(0.6, 0.3), false, false);
    assert_eq!(tracker.finish(ms(40)).spaces.actions, [None]);
}

fn grouped_mark_part(interrupt: impl FnOnce(&mut Tracker)) -> Option<Rect> {
    let mut tracker = tracker();
    tracker.observe(ms(0), Point::new(0.1, 0.2), true, false);
    tracker.observe(ms(10), Point::new(0.5, 0.2), true, false);
    tracker.observe(ms(20), Point::new(0.5, 0.2), false, false);
    interrupt(&mut tracker);
    tracker.observe(ms(30), Point::new(0.1, 0.4), true, false);
    tracker.observe(ms(40), Point::new(0.5, 0.4), true, false);
    tracker.observe(ms(50), Point::new(0.5, 0.4), false, false);
    let marks = tracker.marks();
    assert_eq!(marks.len(), 1);
    tracker.finish(ms(60)).mark_part(&marks[0])
}

#[test]
fn grouped_marks_cannot_cross_resizes_gaps_or_unknown_samples() {
    assert_eq!(grouped_mark_part(|_| {}), Some(Rect::UNIT));
    let resized = grouped_mark_part(|tracker| assert!(tracker.locate(RESIZED)));
    let resized_back = grouped_mark_part(|tracker| {
        assert!(tracker.locate(RESIZED));
        assert!(tracker.locate(Rect::UNIT));
    });
    let relocated_after_gap = grouped_mark_part(|tracker| {
        tracker.gap(ms(25));
        assert!(tracker.locate(Rect::UNIT));
    });
    let unknown_after_gap = grouped_mark_part(|tracker| tracker.gap(ms(25)));
    assert_eq!(
        [
            resized,
            resized_back,
            relocated_after_gap,
            unknown_after_gap
        ],
        [None; 4]
    );
}

#[test]
fn a_cut_preserves_the_carried_points_space_before_folding_the_new_point() {
    let mut tracker = tracker();
    for index in 0..4800 {
        if index == 2400 {
            assert!(tracker.locate(RESIZED));
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
    assert_eq!(parts, [Some(Rect::UNIT), None, Some(RESIZED)]);
}

#[test]
fn mixed_strokes_and_unbound_proof_vectors_never_supply_crop_coordinates()
-> Result<(), serde_json::Error> {
    let mut tracker = tracker();
    tracker.observe(ms(0), Point::new(0.1, 0.2), true, false);
    tracker.observe(ms(10), Point::new(0.3, 0.2), true, false);
    assert!(tracker.locate(RESIZED));
    tracker.observe(ms(20), Point::new(0.6, 0.2), true, false);
    tracker.observe(ms(30), Point::new(0.6, 0.2), false, false);
    let mark = tracker.marks().remove(0);
    let mut recording = tracker.finish(ms(40));
    assert_eq!(recording.mark_part(&mark), None);
    recording.spaces.strokes.push(None);
    assert!(!recording.spaces_valid());
    assert_eq!(recording.mark_part(&mark), None);
    let bad: Spaces = serde_json::from_value(
        serde_json::json!({"strokes":[{"generation":1,"part":[0,0,2,1]}],"pins":[],"actions":[]}),
    )?;
    assert!(!bad.valid(1, 0, 0, ms(40)));
    Ok(())
}
