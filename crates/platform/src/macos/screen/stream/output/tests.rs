use super::*;

#[test]
fn calibration_never_backdates_a_native_frame_and_rejects_scheduler_gaps() {
    let before = Instant::now();
    let after = before.checked_add(Duration::from_micros(700)).unwrap();

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
