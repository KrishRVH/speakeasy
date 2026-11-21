use std::time::Duration;

use super::{FilterKeys, Repeat};

const OFF: FilterKeys = FilterKeys {
    on: false,
    wait: 0,
    delay: 0,
    repeat: 0,
    bounce: 0,
};

fn after(delay: u64, period: u64) -> Repeat {
    Repeat::After {
        delay: Duration::from_millis(delay),
        period: Duration::from_millis(period),
    }
}

#[test]
fn repeat_follows_the_keyboard_settings_and_filter_keys_when_it_is_on() {
    let repeat_keys = FilterKeys {
        on: true,
        wait: 500,
        delay: 2_000,
        repeat: 1_000,
        bounce: 0,
    };
    let bounce_keys = FilterKeys {
        on: true,
        bounce: 1_000,
        ..OFF
    };
    for (delay, speed, filter, expected, case) in [
        (
            Some(1),
            Some(31),
            Some(OFF),
            after(500, 33),
            "Windows' defaults",
        ),
        (
            Some(3),
            Some(0),
            Some(OFF),
            after(1_000, 400),
            "the slowest keyboard settings",
        ),
        (
            Some(0),
            Some(15),
            Some(OFF),
            after(250, 63),
            "a middle speed",
        ),
        (
            None,
            None,
            Some(OFF),
            after(1_000, 400),
            "unreadable keyboard settings",
        ),
        (
            Some(1),
            Some(31),
            None,
            after(20_000, 20_000),
            "unreadable FilterKeys",
        ),
        (
            Some(1),
            Some(31),
            Some(repeat_keys),
            after(2_500, 1_000),
            "RepeatKeys and SlowKeys",
        ),
        (
            Some(1),
            Some(31),
            Some(FilterKeys {
                repeat: 0,
                ..repeat_keys
            }),
            Repeat::Never,
            "RepeatKeys without repeat",
        ),
        (
            Some(1),
            Some(31),
            Some(FilterKeys {
                on: false,
                ..repeat_keys
            }),
            after(500, 33),
            "FilterKeys turned off",
        ),
        (
            Some(1),
            Some(31),
            Some(bounce_keys),
            after(500, 33),
            "BounceKeys",
        ),
    ] {
        assert_eq!(
            Repeat::from_settings(delay, speed, filter),
            expected,
            "{case}"
        );
    }
    assert_eq!(
        after(500, 33).quiet_limit(),
        Some(Duration::from_millis(1_250))
    );
    assert_eq!(
        after(250, 400).quiet_limit(),
        Some(Duration::from_millis(1_050))
    );
    assert_eq!(Repeat::Never.quiet_limit(), None);
}

#[cfg(target_os = "windows")]
#[test]
fn this_desktop_reports_every_repeat_setting() {
    let (delay, speed, filter) = super::settings();
    assert!(
        delay.is_some() && speed.is_some(),
        "The keyboard repeat settings were unreadable"
    );
    assert!(filter.is_some(), "The FilterKeys settings were unreadable");
}
