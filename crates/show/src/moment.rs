//! Points and stretches on a Show's timeline.

use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};

/// A point on a Show's timeline, in whole milliseconds since recording began.
///
/// `Moment` is to a Show what [`std::time::Instant`] is to the process clock: points are moments
/// and the distances between them are [`Duration`]s. Arithmetic saturates at the timeline's ends.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Moment(u64);

impl Moment {
    /// The moment recording began.
    pub const START: Self = Self(0);

    /// The moment `millis` milliseconds after recording began.
    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    /// Milliseconds since recording began.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Time elapsed from `earlier` to `self`, or zero when `earlier` is the later moment.
    #[must_use]
    pub const fn since(self, earlier: Self) -> Duration {
        Duration::from_millis(self.0.saturating_sub(earlier.0))
    }

    /// Time between two moments in either order.
    #[must_use]
    pub const fn distance(self, other: Self) -> Duration {
        Duration::from_millis(self.0.abs_diff(other.0))
    }

    /// The moment `duration` later.
    #[must_use]
    pub fn saturating_add(self, duration: Duration) -> Self {
        Self(self.0.saturating_add(millis(duration)))
    }

    /// The moment `duration` earlier, or the start.
    #[must_use]
    pub fn saturating_sub(self, duration: Duration) -> Self {
        Self(self.0.saturating_sub(millis(duration)))
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl From<Duration> for Moment {
    fn from(elapsed: Duration) -> Self {
        Self(millis(elapsed))
    }
}

/// Formats as `M:SS`.
impl fmt::Display for Moment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{:02}", self.0 / 60_000, self.0 / 1_000 % 60)
    }
}

/// A stretch of the timeline whose start never follows its end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "[Moment; 2]", into = "[Moment; 2]")]
pub struct Span {
    start: Moment,
    end: Moment,
}

impl Span {
    /// The stretch between two moments, in either order.
    #[must_use]
    pub fn new(one: Moment, other: Moment) -> Self {
        Self {
            start: one.min(other),
            end: one.max(other),
        }
    }

    /// A stretch holding only `moment`.
    #[must_use]
    pub const fn at(moment: Moment) -> Self {
        Self {
            start: moment,
            end: moment,
        }
    }

    /// Where the stretch begins.
    #[must_use]
    pub const fn start(self) -> Moment {
        self.start
    }

    /// Where the stretch ends.
    #[must_use]
    pub const fn end(self) -> Moment {
        self.end
    }

    /// How long the stretch lasts.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.end.since(self.start)
    }

    /// Whether `moment` lies within the stretch, ends included.
    #[must_use]
    pub fn contains(self, moment: Moment) -> bool {
        self.start <= moment && moment <= self.end
    }

    /// The smallest stretch covering both.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

impl From<[Moment; 2]> for Span {
    fn from([one, other]: [Moment; 2]) -> Self {
        Self::new(one, other)
    }
}

impl From<Span> for [Moment; 2] {
    fn from(span: Span) -> Self {
        [span.start, span.end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moments_read_as_minutes_and_seconds() {
        assert_eq!(Moment::from_millis(125_999).to_string(), "2:05");
        assert_eq!(Moment::from_millis(3_600_000).to_string(), "60:00");
        assert_eq!(Moment::from(Duration::from_secs(9)).to_string(), "0:09");
    }

    #[test]
    fn arithmetic_saturates_at_the_timeline_ends() {
        let early = Moment::from_millis(100);
        assert_eq!(early.saturating_sub(Duration::from_secs(1)), Moment::START);
        assert_eq!(Moment::START.since(early), Duration::ZERO);
        assert_eq!(early.distance(Moment::START), Duration::from_millis(100));
        assert_eq!(
            Moment::from_millis(u64::MAX).saturating_add(Duration::MAX),
            Moment::from_millis(u64::MAX)
        );
    }

    #[test]
    fn spans_never_run_backwards_even_when_deserialized() {
        let span: Span = serde_json::from_str("[900, 100]").unwrap();
        assert_eq!(
            (span.start().as_millis(), span.end().as_millis()),
            (100, 900)
        );
        assert_eq!(serde_json::to_string(&span).unwrap(), "[100,900]");
        assert_eq!(span.duration(), Duration::from_millis(800));
        assert!(span.contains(Moment::from_millis(900)) && !span.contains(Moment::START));
    }
}
