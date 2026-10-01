use std::time::{Duration, Instant};

/// Maximum duration of one recording, including microphone startup.
pub const RECORDING_LIMIT: Duration = Duration::from_secs(300);
const TAP: Duration = Duration::from_millis(220);
const DOUBLE_TAP: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Logical dictation mode, independent of the physical shortcut state.
pub enum State {
    #[default]
    /// No recording or recognition is active.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Work requested by a gesture transition.
pub enum Action {
    /// Open a new recording.
    Start,
    /// Stop capture and recognize its audio.
    Finish,
    /// Discard the recording and revoke its insertion authority.
    Cancel,
    /// Refresh the displayed capture mode without replacing the recording.
    ModeChanged,
}

/// Physical key state survives completion and cancellation, preventing autorepeat
/// from starting a new session before the user releases the shortcut.
#[derive(Default)]
pub struct Gesture {
    state: State,
    started: Option<Instant>,
    down: bool,
    pressed: Option<Instant>,
    released: Option<Instant>,
}

impl Gesture {
    /// Current mode; only gesture transitions may change it.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// Observe a physical press; autorepeat cannot open another recording.
    pub fn press(&mut self, now: Instant) -> Option<Action> {
        let expired = self.tick(now);
        if self.down {
            return expired;
        }
        self.down = true;
        self.pressed = Some(now);
        if expired.is_some() {
            return expired;
        }
        match self.state {
            State::Idle => {
                self.started = Some(now);
                self.state = State::Held;
                Some(Action::Start)
            },
            State::PendingTap => {
                self.state = State::HandsFree;
                Some(Action::ModeChanged)
            },
            State::HandsFree => self.finish(),
            _ => None,
        }
    }

    /// Observe a release, finishing a hold or waiting for a second tap.
    pub fn release(&mut self, now: Instant) -> Option<Action> {
        let expired = self.tick(now);
        let was_down = std::mem::replace(&mut self.down, false);
        if expired.is_some() || !was_down || self.state != State::Held {
            return expired;
        }
        if self.pressed.is_some_and(|at| now.duration_since(at) <= TAP) {
            self.released = Some(now);
            self.state = State::PendingTap;
            Some(Action::ModeChanged)
        } else {
            self.finish()
        }
    }

    /// Space during a hold keeps the capture and switches it to hands-free.
    pub fn lock(&mut self) -> Option<Action> {
        if self.state != State::Held {
            return None;
        }
        self.state = State::HandsFree;
        Some(Action::ModeChanged)
    }

    /// Start hands-free recording or finish the active recording.
    pub fn toggle(&mut self, now: Instant) -> Option<Action> {
        match self.state {
            State::Idle => {
                self.state = State::HandsFree;
                self.started = Some(now);
                Some(Action::Start)
            },
            State::Processing => None,
            _ => self.finish(),
        }
    }

    /// Earliest tap or recording deadline; callers wake and tick at this instant.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        if matches!(self.state, State::Idle | State::Processing) {
            return None;
        }
        let started = self.started?;
        // An unrepresentable deadline expires immediately rather than removing the cap.
        let limit = started.checked_add(RECORDING_LIMIT).unwrap_or(started);
        Some(if self.state == State::PendingTap {
            self.released.map_or(limit, |at| {
                limit.min(at.checked_add(DOUBLE_TAP).unwrap_or(at))
            })
        } else {
            limit
        })
    }

    /// Finish an expired recording or tap without consulting an ambient clock.
    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.finish()
        } else {
            None
        }
    }

    /// Move active capture into processing without changing physical key state.
    pub fn finish(&mut self) -> Option<Action> {
        if matches!(self.state, State::Idle | State::Processing) {
            return None;
        }
        self.state = State::Processing;
        Some(Action::Finish)
    }

    /// Abandon capture or processing; a held shortcut still requires release.
    pub fn cancel(&mut self) -> Option<Action> {
        if self.state == State::Idle {
            return None;
        }
        self.state = State::Idle;
        self.started = None;
        Some(Action::Cancel)
    }

    /// Acknowledge processing completion; an active recording is unaffected.
    pub fn complete(&mut self) {
        if self.state == State::Processing {
            self.state = State::Idle;
            self.started = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    proptest::proptest! {
        #[test]
        fn arbitrary_event_sequences_keep_capture_bounded(
            events in proptest::collection::vec((0_u8..7, 0_u32..600_000), 0..128)
        ) {
            let mut gesture = Gesture::default();
            let mut now = Instant::now();
            for (event, elapsed) in events {
                now = now.checked_add(Duration::from_millis(u64::from(elapsed)))
                    .expect("The generated sequence spans less than one day");
                match event {
                    0 => { gesture.press(now); }
                    1 => { gesture.release(now); }
                    2 => { gesture.lock(); }
                    3 => { gesture.toggle(now); }
                    4 => { gesture.cancel(); }
                    5 => { gesture.complete(); }
                    _ => { gesture.finish(); }
                }
                // The owner ticks after every observation, including unrelated wakeups.
                gesture.tick(now);
                if matches!(gesture.state(), State::Held | State::PendingTap | State::HandsFree) {
                    let started = gesture.started.expect("Active capture retains its start");
                    let limit = started.checked_add(RECORDING_LIMIT).expect("Bounded test clock");
                    let deadline = gesture.deadline().expect("Active capture retains its deadline");
                    proptest::prop_assert!(started <= now && now < deadline && deadline <= limit);
                } else {
                    proptest::prop_assert!(gesture.deadline().is_none());
                    if gesture.state() == State::Idle {
                        proptest::prop_assert!(gesture.started.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn single_tap_finishes_but_double_tap_keeps_the_original_capture() {
        let t = Instant::now();
        let mut g = Gesture::default();
        assert_eq!(g.press(t), Some(Action::Start));
        g.release(t + TAP);
        assert_eq!(g.tick(t + TAP + DOUBLE_TAP), Some(Action::Finish));
        g.complete();
        let t = t + Duration::from_secs(1);
        g.press(t);
        g.release(t + TAP);
        assert_eq!(
            g.press(
                (t + TAP + DOUBLE_TAP)
                    .checked_sub(Duration::from_millis(1))
                    .unwrap()
            ),
            Some(Action::ModeChanged)
        );
        assert_eq!(g.state, State::HandsFree);
        assert_eq!(g.started, Some(t));
    }

    #[test]
    fn locking_a_hold_keeps_its_capture_until_the_next_press() {
        let t = Instant::now();
        let mut g = Gesture::default();
        assert_eq!(g.press(t), Some(Action::Start));
        assert_eq!(g.lock(), Some(Action::ModeChanged));
        assert_eq!(g.release(t + Duration::from_secs(1)), None);
        assert_eq!(g.state, State::HandsFree);
        assert_eq!(g.started, Some(t));
        assert_eq!(g.lock(), None);
        assert_eq!(g.press(t + Duration::from_secs(2)), Some(Action::Finish));
    }

    #[test]
    fn expiration_wins_over_second_press_and_capture_is_capped() {
        let t = Instant::now();
        let mut g = Gesture::default();
        g.press(t);
        g.release(t + TAP);
        assert_eq!(g.press(t + TAP + DOUBLE_TAP), Some(Action::Finish));
        assert_eq!(g.state, State::Processing);
        g.complete();
        assert_eq!(g.press(t + Duration::from_secs(1)), None);
        g.release(t + Duration::from_secs(1));
        g.press(t + Duration::from_secs(2));
        assert_eq!(g.tick(t + Duration::from_secs(302)), Some(Action::Finish));
        assert_eq!(g.release(t + Duration::from_secs(303)), None);
    }

    #[test]
    fn cancel_requires_release_and_late_completion_cannot_end_new_capture() {
        let t = Instant::now();
        let mut g = Gesture::default();
        g.press(t);
        assert_eq!(g.cancel(), Some(Action::Cancel));
        assert_eq!(g.press(t), None);
        g.release(t);
        assert_eq!(g.press(t), Some(Action::Start));
        g.complete();
        assert_eq!(g.state, State::Held);
    }
}
