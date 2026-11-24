use std::{
    mem,
    time::{Duration, Instant},
};

/// Counted from the press, so microphone startup is included.
pub const RECORDING_LIMIT: Duration = Duration::from_secs(300);

/// A press released this soon is a tap, which may start a double-tap; a longer one is a hold.
const TAP: Duration = Duration::from_millis(300);

const DOUBLE_TAP: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Moment(u64);

impl Moment {
    #[must_use]
    pub fn since(origin: Instant, now: Instant) -> Self {
        Self(nanos(now.saturating_duration_since(origin)))
    }

    #[must_use]
    pub const fn nanos(self) -> u64 {
        self.0
    }

    #[must_use]
    pub(crate) const fn after(self, nanos: u64) -> Self {
        Self(self.0.saturating_add(nanos))
    }

    const fn elapsed_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    tap: u64,
    double_tap: u64,
    limit: u64,
}

impl Timing {
    pub const DICTATION: Self = Self {
        tap: nanos(TAP),
        double_tap: nanos(DOUBLE_TAP),
        limit: nanos(RECORDING_LIMIT),
    };

    #[cfg(test)]
    pub(crate) const fn new(tap: u64, double_tap: u64, limit: u64) -> Self {
        Self {
            tap,
            double_tap,
            limit,
        }
    }
}

impl Default for Timing {
    fn default() -> Self {
        Self::DICTATION
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Idle,
    Held,
    PendingTap,
    HandsFree,
    Processing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Start,
    Finish,
    Cancel,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Gesture {
    phase: Phase,
    /// Survives completion and cancellation, so autorepeat cannot start a new session early.
    down: bool,
    timing: Timing,
}

impl Gesture {
    #[must_use]
    pub(crate) fn new(timing: Timing) -> Self {
        Self {
            timing,
            ..Self::default()
        }
    }

    pub(crate) fn deliver(&mut self) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.phase = Phase::Processing;
        true
    }

    /// Enters saving from idle for a Show recovery, which Escape and cancel then leave alone.
    pub(crate) fn save(&mut self) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.phase = Phase::Saving;
        true
    }

    #[must_use]
    pub(crate) fn state(&self) -> State {
        match self.phase {
            Phase::Idle => State::Idle,
            Phase::Held { .. } => State::Held,
            Phase::PendingTap { .. } => State::PendingTap,
            Phase::HandsFree { .. } | Phase::Show { .. } | Phase::ShowArmed { .. } => {
                State::HandsFree
            },
            Phase::Processing | Phase::Saving => State::Processing,
        }
    }

    pub(crate) fn press(&mut self, now: Moment) -> Option<Action> {
        let expired = self.tick(now);
        let already_down = mem::replace(&mut self.down, true);
        if already_down || expired.is_some() {
            return expired;
        }
        match self.phase {
            Phase::Idle => {
                self.phase = Phase::Held { started: now };
                Some(Action::Start)
            },
            Phase::PendingTap { started, .. } => {
                self.phase = Phase::HandsFree { started };
                None
            },
            Phase::HandsFree { .. } => self.finish(),
            Phase::Show { started } => {
                self.phase = Phase::ShowArmed { started };
                None
            },
            Phase::Held { .. } | Phase::ShowArmed { .. } | Phase::Processing | Phase::Saving => {
                None
            },
        }
    }

    pub(crate) fn release(&mut self, now: Moment) -> Option<Action> {
        let expired = self.tick(now);
        self.down = false;
        match self.phase {
            Phase::Held { started } if now.elapsed_since(started) <= self.timing.tap => {
                self.phase = Phase::PendingTap {
                    started,
                    released: now,
                };
                None
            },
            Phase::Held { .. } | Phase::ShowArmed { .. } => self.finish(),
            Phase::Idle
            | Phase::PendingTap { .. }
            | Phase::HandsFree { .. }
            | Phase::Show { .. }
            | Phase::Processing
            | Phase::Saving => expired,
        }
    }

    pub(crate) fn submit(&mut self) -> Option<Action> {
        if matches!(self.phase, Phase::Held { .. }) {
            self.finish()
        } else {
            None
        }
    }

    pub(crate) fn lock(&mut self) {
        if let Phase::Held { started } = self.phase {
            self.phase = Phase::HandsFree { started };
        }
    }

    /// A Show records hands-free, so releasing the shortcut that started it does not finish it.
    pub(crate) fn show(&mut self) {
        if let Phase::Held { started } | Phase::HandsFree { started } = self.phase {
            self.phase = Phase::Show { started };
        }
    }

    pub(crate) fn toggle(&mut self, now: Moment) -> Option<Action> {
        match self.phase {
            Phase::Idle => {
                self.phase = Phase::HandsFree { started: now };
                Some(Action::Start)
            },
            Phase::Held { .. }
            | Phase::PendingTap { .. }
            | Phase::HandsFree { .. }
            | Phase::Show { .. }
            | Phase::ShowArmed { .. } => self.finish(),
            Phase::Processing | Phase::Saving => None,
        }
    }

    #[must_use]
    pub(crate) fn deadline(&self) -> Option<Moment> {
        match self.phase {
            Phase::Idle | Phase::Processing | Phase::Saving => None,
            Phase::Held { started }
            | Phase::HandsFree { started }
            | Phase::Show { started }
            | Phase::ShowArmed { started } => Some(started.after(self.timing.limit)),
            Phase::PendingTap { started, released } => Some(
                started
                    .after(self.timing.limit)
                    .min(released.after(self.timing.double_tap)),
            ),
        }
    }

    pub(crate) fn tick(&mut self, now: Moment) -> Option<Action> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.finish()
        } else {
            None
        }
    }

    pub(crate) fn finish(&mut self) -> Option<Action> {
        self.phase = match self.phase {
            Phase::Idle | Phase::Processing | Phase::Saving => return None,
            Phase::Held { .. } | Phase::PendingTap { .. } | Phase::HandsFree { .. } => {
                Phase::Processing
            },
            Phase::Show { .. } | Phase::ShowArmed { .. } => Phase::Saving,
        };
        Some(Action::Finish)
    }

    /// Escape also reaches the focused app, so it cancels a Show only while the shortcut is held.
    pub(crate) fn escape(&mut self) -> Option<Action> {
        match self.phase {
            Phase::Show { .. } | Phase::ShowArmed { .. } if !self.down => None,
            _ => self.cancel(),
        }
    }

    /// An interruption the user never asked for discards dictation but finishes a recording Show.
    pub(crate) fn interrupt(&mut self) -> Option<Action> {
        match self.phase {
            Phase::Show { .. } | Phase::ShowArmed { .. } => self.finish(),
            _ => self.cancel(),
        }
    }

    /// Cancels anything but a Show being saved; only `abandon` discards that.
    pub(crate) fn cancel(&mut self) -> Option<Action> {
        if self.phase == Phase::Saving {
            return None;
        }
        self.abandon()
    }

    pub(crate) fn abandon(&mut self) -> Option<Action> {
        if self.phase == Phase::Idle {
            return None;
        }
        self.phase = Phase::Idle;
        Some(Action::Cancel)
    }

    pub(crate) fn complete(&mut self) {
        if matches!(self.phase, Phase::Processing | Phase::Saving) {
            self.phase = Phase::Idle;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Held {
        started: Moment,
    },
    PendingTap {
        started: Moment,
        released: Moment,
    },
    HandsFree {
        started: Moment,
    },
    Show {
        started: Moment,
    },
    ShowArmed {
        started: Moment,
    },
    Processing,
    Saving,
}

const fn nanos(duration: Duration) -> u64 {
    let whole = duration.as_secs().saturating_mul(1_000_000_000);
    whole.saturating_add(duration.subsec_nanos() as u64)
}

#[cfg(test)]
mod tests;
