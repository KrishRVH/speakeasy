use super::*;

#[test]
fn native_frames_map_onto_the_host_clock_and_never_land_in_the_future() {
    let anchor = Instant::now();
    let clock = Clock {
        origin: Duration::from_secs(1),
        instant: anchor,
    };
    let later = unsafe { CMClock::convert_host_time_to_system_units(CMTime::new(2, 1)) };
    let now = anchor.checked_add(Duration::from_secs(2)).unwrap();
    assert_eq!(
        clock.at(later, now),
        anchor.checked_add(Duration::from_secs(1))
    );
    assert!(clock.at(later, anchor).is_none());
}
