use std::time::Duration;

pub(super) const VSYNC_INTERVAL_THRESHOLD: Duration = Duration::from_millis(1);

pub(super) fn select_interval(
    qpc_period: u64,
    qpc_frequency: u64,
    refresh_numerator: u32,
    refresh_denominator: u32,
) -> Option<Duration> {
    retrieve_duration(qpc_period, qpc_frequency)
        .filter(|interval| *interval >= VSYNC_INTERVAL_THRESHOLD)
        .or_else(|| {
            retrieve_duration(u64::from(refresh_denominator), u64::from(refresh_numerator))
                .filter(|interval| *interval >= VSYNC_INTERVAL_THRESHOLD)
        })
}

fn retrieve_duration(counts: u64, ticks_per_second: u64) -> Option<Duration> {
    let seconds = counts.checked_div(ticks_per_second)?;
    // Scale the remainder before division to preserve fractional clock rates.
    // The u64 remainder times 1e9 fits in u128; the result is below 1e9.
    let remainder = counts.checked_rem(ticks_per_second)?;
    let nanoseconds = u128::from(remainder)
        .checked_mul(1_000_000_000)?
        .checked_div(u128::from(ticks_per_second))?;
    let nanoseconds = u32::try_from(nanoseconds).ok()?;
    Some(Duration::new(seconds, nanoseconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_refresh_ratios_produce_the_same_interval() {
        let expected = Some(Duration::from_nanos(16_666_666));
        for (numerator, denominator) in [(60, 1), (60_000, 1_000), (60_000_000, 1_000_000)] {
            assert_eq!(
                select_interval(60, 10_000_000, numerator, denominator),
                expected,
                "refresh ratio {numerator}/{denominator}"
            );
        }
    }

    #[test]
    fn fractional_refresh_rate_keeps_nanosecond_precision() {
        assert_eq!(
            select_interval(60, 10_000_000, 60_000, 1_001),
            Some(Duration::from_nanos(16_683_333))
        );
    }

    #[test]
    fn qpc_frequency_is_not_truncated_before_conversion() {
        assert_eq!(
            select_interval(52_083, 3_125_000, 120, 1),
            Some(Duration::from_nanos(16_666_560))
        );
        assert_eq!(
            select_interval(166_666, 10_000_000, 120, 1),
            Some(Duration::from_nanos(16_666_600))
        );
    }

    #[test]
    fn interval_validation_uses_the_precise_qpc_duration() {
        assert_eq!(
            select_interval(3_000, 3_125_000, 60, 1),
            Some(Duration::from_nanos(16_666_666))
        );
    }

    #[test]
    fn valid_qpc_period_has_priority_over_invalid_refresh_rates() {
        assert_eq!(
            select_interval(10_000, 1_000_000, 0, 0),
            Some(Duration::from_millis(10))
        );
        assert_eq!(
            select_interval(1, 1_000, 0, 0),
            Some(VSYNC_INTERVAL_THRESHOLD)
        );
    }

    #[test]
    fn invalid_qpc_frequency_can_use_a_valid_refresh_rate() {
        assert_eq!(
            select_interval(60, 0, 60, 1),
            Some(Duration::from_nanos(16_666_666))
        );
    }

    #[test]
    fn zero_and_implausibly_short_fallback_intervals_are_rejected() {
        for (numerator, denominator) in [(0, 0), (0, 1), (60, 0), (2_000, 1), (10_000, 1)] {
            assert_eq!(
                select_interval(0, 10_000_000, numerator, denominator),
                None,
                "refresh ratio {numerator}/{denominator}"
            );
        }
        assert_eq!(select_interval(0, 0, 0, 0), None);
        assert_eq!(
            select_interval(0, 10_000_000, 1_000, 1),
            Some(VSYNC_INTERVAL_THRESHOLD)
        );
    }

    #[test]
    fn conversion_handles_zero_and_u64_boundaries_without_overflow() {
        assert_eq!(retrieve_duration(1, 0), None);
        assert_eq!(retrieve_duration(0, 1), Some(Duration::ZERO));
        assert_eq!(
            retrieve_duration(u64::MAX, 1),
            Some(Duration::from_secs(u64::MAX))
        );
        assert_eq!(
            retrieve_duration(u64::MAX, 2),
            Some(Duration::new(u64::MAX / 2, 500_000_000))
        );
        assert_eq!(
            retrieve_duration(u64::MAX, u64::MAX),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            retrieve_duration(u64::MAX - 1, u64::MAX),
            Some(Duration::from_nanos(999_999_999))
        );
    }
}
