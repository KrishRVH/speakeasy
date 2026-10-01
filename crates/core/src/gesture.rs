//! Shortcut transitions with explicit timestamps and bounded recording deadlines.
//!
//! Every capture ends by [`RECORDING_LIMIT`], and no transition consults an ambient clock.

use std::{
    mem,
    time::{Duration, Instant},
};

/// Maximum duration of one recording, including microphone startup.
pub const RECORDING_LIMIT: Duration = Duration::from_secs(300);
/// A hold released within this long of its press is a tap.
const TAP: Duration = Duration::from_millis(220);
/// How long after a tap's release a second press switches to hands-free.
const DOUBLE_TAP: Duration = Duration::from_millis(300);

/// Logical dictation mode, independent of the physical shortcut state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// No recording or recognition is active.
    #[default]
    Idle,
    /// Recording while the shortcut is held.
    Held,
    /// A short release is waiting for a possible second tap.
    PendingTap,
    /// Recording until the next press, cancellation, or deadline.
    HandsFree,
    /// Capture is complete; recognition or insertion is pending.
    Processing,
}

/// Work requested by a gesture transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Opens a new recording.
    Start,
    /// Stops capture and recognizes its audio.
    Finish,
    /// Discards the recording and revokes its insertion authority.
    Cancel,
    /// Refreshes the displayed capture mode without replacing the recording.
    ModeChanged,
}

/// Hold-to-talk, double-tap hands-free, and Space locking for one shortcut.
///
/// Physical key state survives completion and cancellation, preventing autorepeat from starting a
/// new session before the user releases the shortcut.
#[derive(Default)]
pub struct Gesture {
    phase: Phase,
    down: bool,
}

impl Gesture {
    /// Returns the current mode; only gesture transitions change it.
    #[must_use]
    pub fn state(&self) -> State {
        match self.phase {
            Phase::Idle => State::Idle,
            Phase::Held { .. } => State::Held,
            Phase::PendingTap { .. } => State::PendingTap,
            Phase::HandsFree { .. } => State::HandsFree,
            Phase::Processing => State::Processing,
        }
    }

    /// Observes a physical press; autorepeat cannot open another recording.
    pub fn press(&mut self, now: Instant) -> Option<Action> {
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
                Some(Action::ModeChanged)
            },
            Phase::HandsFree { .. } => self.finish(),
            Phase::Held { .. } | Phase::Processing => None,
        }
    }

    /// Observes a release, finishing a hold or waiting for a second tap.
    pub fn release(&mut self, now: Instant) -> Option<Action> {
        let expired = self.tick(now);
        self.down = false;
        let Phase::Held { started } = self.phase else {
            return expired;
        };
        if now.duration_since(started) <= TAP {
            self.phase = Phase::PendingTap {
                started,
                released: now,
            };
            Some(Action::ModeChanged)
        } else {
            self.finish()
        }
    }

    /// Switches a hold to hands-free on Space, keeping its capture.
    pub fn lock(&mut self) -> Option<Action> {
        let Phase::Held { started } = self.phase else {
            return None;
        };
        self.phase = Phase::HandsFree { started };
        Some(Action::ModeChanged)
    }

    /// Starts hands-free recording or finishes the active recording.
    pub fn toggle(&mut self, now: Instant) -> Option<Action> {
        match self.phase {
            Phase::Idle => {
                self.phase = Phase::HandsFree { started: now };
                Some(Action::Start)
            },
            Phase::Held { .. } | Phase::PendingTap { .. } | Phase::HandsFree { .. } => {
                self.finish()
            },
            Phase::Processing => None,
        }
    }

    /// Returns the earliest tap or recording deadline; callers wake and tick at this instant.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Idle | Phase::Processing => None,
            Phase::Held { started } | Phase::HandsFree { started } => {
                Some(expiry(started, RECORDING_LIMIT))
            },
            Phase::PendingTap { started, released } => {
                Some(expiry(started, RECORDING_LIMIT).min(expiry(released, DOUBLE_TAP)))
            },
        }
    }

    /// Finishes an expired recording or tap without consulting an ambient clock.
    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.finish()
        } else {
            None
        }
    }

    /// Moves active capture into processing without changing physical key state.
    pub fn finish(&mut self) -> Option<Action> {
        match self.phase {
            Phase::Idle | Phase::Processing => None,
            Phase::Held { .. } | Phase::PendingTap { .. } | Phase::HandsFree { .. } => {
                self.phase = Phase::Processing;
                Some(Action::Finish)
            },
        }
    }

    /// Abandons capture or processing; a held shortcut still requires release.
    pub fn cancel(&mut self) -> Option<Action> {
        if self.phase == Phase::Idle {
            return None;
        }
        self.phase = Phase::Idle;
        Some(Action::Cancel)
    }

    /// Acknowledges processing completion; an active recording is unaffected.
    pub fn complete(&mut self) {
        if self.phase == Phase::Processing {
            self.phase = Phase::Idle;
        }
    }
}

/// A [`State`] with the instants its deadlines count from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Held {
        started: Instant,
    },
    PendingTap {
        started: Instant,
        released: Instant,
    },
    HandsFree {
        started: Instant,
    },
    Processing,
}

/// `start + after`, or `start` itself when that instant is unrepresentable, so an overflowing
/// deadline expires immediately rather than removing the cap.
fn expiry(start: Instant, after: Duration) -> Instant {
    start.checked_add(after).unwrap_or(start)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum Event {
        Press,
        Release,
        Lock,
        Toggle,
        Cancel,
        Complete,
        Finish,
    }

    impl Event {
        const ALL: &[Self] = &[
            Self::Press,
            Self::Release,
            Self::Lock,
            Self::Toggle,
            Self::Cancel,
            Self::Complete,
            Self::Finish,
        ];

        fn apply(self, gesture: &mut Gesture, now: Instant) -> Option<Action> {
            match self {
                Self::Press => gesture.press(now),
                Self::Release => gesture.release(now),
                Self::Lock => gesture.lock(),
                Self::Toggle => gesture.toggle(now),
                Self::Cancel => gesture.cancel(),
                Self::Complete => {
                    gesture.complete();
                    None
                },
                Self::Finish => gesture.finish(),
            }
        }
    }

    fn capture_start(gesture: &Gesture) -> Option<Instant> {
        match gesture.phase {
            Phase::Held { started }
            | Phase::PendingTap { started, .. }
            | Phase::HandsFree { started } => Some(started),
            Phase::Idle | Phase::Processing => None,
        }
    }

    #[test]
    fn unrepresentable_recording_deadline_expires_immediately() {
        let mut latest = Instant::now();
        for shift in (0..64).rev() {
            let seconds = 1_u64.checked_shl(shift).expect("Shift is below 64");
            if let Some(next) = latest.checked_add(Duration::from_secs(seconds)) {
                latest = next;
            }
        }
        assert!(latest.checked_add(RECORDING_LIMIT).is_none());
        let mut gesture = Gesture::default();
        assert_eq!(gesture.press(latest), Some(Action::Start));
        assert_eq!(gesture.deadline(), Some(latest));
        assert_eq!(gesture.tick(latest), Some(Action::Finish));
    }

    proptest! {
        #[test]
        fn arbitrary_event_sequences_keep_capture_bounded(
            events in prop::collection::vec(
                (prop::sample::select(Event::ALL), 0_u32..600_000),
                0..128,
            )
        ) {
            let mut gesture = Gesture::default();
            let mut now = Instant::now();
            for (event, elapsed) in events {
                now = now
                    .checked_add(Duration::from_millis(u64::from(elapsed)))
                    .expect("The generated sequence spans less than one day");
                event.apply(&mut gesture, now);
                // The owner ticks after every observation, including unrelated wakeups.
                gesture.tick(now);
                if let Some(started) = capture_start(&gesture) {
                    let limit = started.checked_add(RECORDING_LIMIT).expect("Bounded test clock");
                    let deadline = gesture.deadline().expect("Active capture retains its deadline");
                    prop_assert!(started <= now && now < deadline && deadline <= limit);
                } else {
                    prop_assert!(gesture.deadline().is_none());
                }
            }
        }
    }

    #[test]
    fn single_tap_finishes_but_double_tap_keeps_the_original_capture() {
        let start = Instant::now();
        let mut gesture = Gesture::default();
        assert_eq!(gesture.press(start), Some(Action::Start));
        gesture.release(start + TAP);
        assert_eq!(gesture.tick(start + TAP + DOUBLE_TAP), Some(Action::Finish));
        gesture.complete();
        let start = start + Duration::from_secs(1);
        gesture.press(start);
        gesture.release(start + TAP);
        let just_before_expiry = (start + TAP + DOUBLE_TAP)
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        assert_eq!(gesture.press(just_before_expiry), Some(Action::ModeChanged));
        assert_eq!(gesture.phase, Phase::HandsFree { started: start });
    }

    #[test]
    fn locking_a_hold_keeps_its_capture_until_the_next_press() {
        let start = Instant::now();
        let mut gesture = Gesture::default();
        assert_eq!(gesture.press(start), Some(Action::Start));
        assert_eq!(gesture.lock(), Some(Action::ModeChanged));
        assert_eq!(gesture.release(start + Duration::from_secs(1)), None);
        assert_eq!(gesture.phase, Phase::HandsFree { started: start });
        assert_eq!(gesture.lock(), None);
        assert_eq!(
            gesture.press(start + Duration::from_secs(2)),
            Some(Action::Finish)
        );
    }

    #[test]
    fn expiration_wins_over_second_press_and_capture_is_capped() {
        let start = Instant::now();
        let mut gesture = Gesture::default();
        gesture.press(start);
        gesture.release(start + TAP);
        assert_eq!(
            gesture.press(start + TAP + DOUBLE_TAP),
            Some(Action::Finish)
        );
        assert_eq!(gesture.state(), State::Processing);
        gesture.complete();
        assert_eq!(gesture.press(start + Duration::from_secs(1)), None);
        gesture.release(start + Duration::from_secs(1));
        let second_press = start + Duration::from_secs(2);
        gesture.press(second_press);
        let limit = second_press + RECORDING_LIMIT;
        assert_eq!(gesture.tick(limit), Some(Action::Finish));
        assert_eq!(gesture.release(limit + Duration::from_secs(1)), None);
    }

    #[test]
    fn cancel_requires_release_and_late_completion_cannot_end_new_capture() {
        let start = Instant::now();
        let mut gesture = Gesture::default();
        gesture.press(start);
        assert_eq!(gesture.cancel(), Some(Action::Cancel));
        assert_eq!(gesture.press(start), None);
        gesture.release(start);
        assert_eq!(gesture.press(start), Some(Action::Start));
        gesture.complete();
        assert_eq!(gesture.state(), State::Held);
    }
}
