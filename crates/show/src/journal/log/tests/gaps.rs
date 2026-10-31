use super::*;

#[test]
fn pointer_gaps_survive_replay_without_joining_actions_ink_or_pins() {
    let at = Moment::from_millis;
    let pointer = |ms, x, drawing, pressed| TakeEvent::Pointer {
        part: None,
        at: at(ms),
        point: Point::new(x, 0.5),
        drawing,
        pressed,
    };
    for drawing in [false, true] {
        let mut live = Take::new(0, surface());
        let mut bytes = Log::header(0, surface()).unwrap();
        for event in [
            pointer(10, 0.1, drawing, true),
            pointer(20, 0.2, drawing, true),
            TakeEvent::PointerGap(at(30)),
            pointer(40, 0.8, drawing, true),
            pointer(50, 0.9, drawing, true),
            pointer(60, 0.9, false, false),
            TakeEvent::Pin(Span::new(at(20), at(50))),
            TakeEvent::Pin(Span::new(at(35), at(39))),
            TakeEvent::Pin(Span::new(at(35), at(45))),
            pointer(70, 0.5, false, true),
            pointer(80, 0.5, false, false),
        ] {
            apply(&mut live, &event, &mut bytes);
        }
        for recording in [live.finish().0, replay(&bytes).unwrap().finish().0] {
            assert!(matches!(
                recording.actions.as_slice(),
                [crate::Action::Click { at: when, .. }] if *when == at(80)
            ));
            assert_eq!(recording.pins, []);
            if drawing {
                assert_eq!(recording.strokes.len(), 2);
                assert_eq!(
                    recording.strokes[0].path().collect::<Vec<_>>(),
                    [Point::new(0.1, 0.5), Point::new(0.2, 0.5)]
                );
                assert_eq!(
                    recording.strokes[1].path().collect::<Vec<_>>(),
                    [Point::new(0.8, 0.5), Point::new(0.9, 0.5)]
                );
            }
        }
    }
}
