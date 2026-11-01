use super::*;

#[test]
fn native_source_geometry_cannot_be_clipped_and_relabelled_as_the_selected_display() {
    let display = Bounds {
        origin: [-1000.0, 80.0],
        size: [1000.0, 800.0],
    };
    let captured = Bounds {
        origin: [-800.0, 280.0],
        size: [400.0, 400.0],
    };
    assert_eq!(
        captured_part(display, captured),
        Some(Part::between([0.2, 0.25], [0.6, 0.75]))
    );
    assert!(
        captured_part(
            display,
            Bounds {
                origin: [-1100.0, 80.0],
                ..captured
            }
        )
        .is_none()
    );
    assert!(
        captured_part(
            display,
            Bounds {
                size: [f64::NAN, 400.0],
                ..captured
            }
        )
        .is_none()
    );
}

#[test]
fn calibration_never_backdates_a_native_frame_and_rejects_scheduler_gaps() {
    let before = Instant::now();
    let after = before.checked_add(Duration::from_micros(700)).unwrap();
    // SAFETY: all fixture times are finite, positive host durations with nonzero scales.
    let (origin, later) = unsafe {
        (
            CMTime::new(1, 1),
            CMClock::convert_host_time_to_system_units(CMTime::new(2, 1)),
        )
    };
    let clock = Clock::anchored(origin, before, after).unwrap();
    let now = after.checked_add(Duration::from_secs(2)).unwrap();
    assert_eq!(
        clock.at(later, now),
        after.checked_add(Duration::from_secs(1))
    );
    assert!(clock.at(later, after).is_none());
    assert!(
        Clock::anchored(
            origin,
            before,
            before.checked_add(Duration::from_millis(35)).unwrap()
        )
        .is_none()
    );
}
