//! The timestamp in a Show's name, and the whole-second cutoff used by retention.

use std::time::{Duration, SystemTime};

use super::{KEPT_FOR, stamp};

pub(crate) fn recorded_at(name: &str) -> Option<SystemTime> {
    let stamp = stamp(name)?;
    let number = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<u64>().ok();
    let (year, month, day) = (number(0..4)?, number(4..6)?, number(6..8)?);
    let (hour, minute, second) = (number(9..11)?, number(11..13)?, number(13..15)?);
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let months = [
        31_u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let month_index = usize::try_from(month.checked_sub(1)?).ok()?;
    if day == 0 || day > *months.get(month_index)? || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let preceding = year.checked_sub(1)?;
    let leaps = (preceding / 4)
        .saturating_sub(preceding / 100)
        .saturating_add(preceding / 400);
    let years = year.checked_sub(1_970)?;
    let days = years
        .checked_mul(365)?
        .checked_add(leaps.checked_sub(477)?)?
        .checked_add(months.get(..month_index)?.iter().sum::<u64>())?
        .checked_add(day.checked_sub(1)?)?;
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(hour.checked_mul(3_600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

pub(crate) fn expires_at(name: &str) -> Option<SystemTime> {
    // The sweep compares whole-second names strictly before its cutoff, not at the cutoff.
    recorded_at(name)?
        .checked_add(KEPT_FOR)?
        .checked_add(Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn timestamps_round_trip_and_expiry_matches_the_sweep(seconds in 0_u64..31_556_952_000) {
            let recorded = SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)).unwrap();
            let name = super::super::utc_stamp(recorded);
            prop_assert_eq!(recorded_at(&name), Some(recorded));
            let expiry = expires_at(&name).unwrap();
            let cutoff = |now: SystemTime| super::super::utc_stamp(now.checked_sub(KEPT_FOR).unwrap());
            prop_assert!(name >= cutoff(expiry.checked_sub(Duration::from_secs(1)).unwrap()));
            prop_assert!(name < cutoff(expiry));
        }
    }

    #[test]
    fn invalid_calendar_names_have_no_recorded_time() {
        for name in [
            "20260230T000000Z",
            "20261301T000000Z",
            "20260101T240000Z",
            "19691231T000000Z",
        ] {
            assert!(recorded_at(name).is_none());
        }
        assert!(recorded_at("20240229T235959Z-2").is_some());
    }
}
