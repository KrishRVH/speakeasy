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
fn cancellation_between_primitives_leaves_the_original_frame_untouched() {
    let recording = recording(vec![
        stroke(&[(0.1, 0.3, 100), (0.8, 0.3, 200)]),
        stroke(&[(0.1, 0.7, 300), (0.8, 0.7, 400)]),
    ]);
    let scene = Annotations::new(&recording, &[]).unwrap();
    let mut image = blank();
    let mut polls = 0;
    let result = scene
        .paint(
            &mut image,
            Duration::from_millis(500),
            VIEW,
            &typeface(),
            || {
                polls += 1;
                polls >= 3
            },
        )
        .unwrap();
    assert!(result.is_none());
    assert_eq!(image, blank());
    assert_eq!(polls, 3);
}
