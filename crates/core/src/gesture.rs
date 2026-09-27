use std::time::{Duration, Instant};

pub const RECORDING_LIMIT: Duration = Duration::from_secs(300);
const TAP: Duration = Duration::from_millis(220);
const DOUBLE_TAP: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Idle,
    Held,
    PendingTap,
    HandsFree,
    Processing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Finish,
    Cancel,
    ModeChanged,
}

/// Physical key state survives completion and cancellation, preventing autorepeat
/// from starting a new session before the user releases the shortcut.
#[derive(Default)]
pub struct Gesture {
    pub state: State,
    pub started: Option<Instant>,
    down: bool,
    pressed: Option<Instant>,
    released: Option<Instant>,
}

impl Gesture {
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
            }
            State::PendingTap => {
                self.state = State::HandsFree;
                Some(Action::ModeChanged)
            }
            State::HandsFree => self.finish(),
            _ => None,
        }
    }

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

    pub fn toggle(&mut self, now: Instant) -> Option<Action> {
        match self.state {
            State::Idle => {
                self.state = State::HandsFree;
                self.started = Some(now);
                Some(Action::Start)
            }
            State::Processing => None,
            _ => self.finish(),
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        if matches!(self.state, State::Idle | State::Processing) {
            return None;
        }
        let limit = self.started? + RECORDING_LIMIT;
        Some(if self.state == State::PendingTap {
            self.released.map_or(limit, |at| limit.min(at + DOUBLE_TAP))
        } else {
            limit
        })
    }

    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.finish()
        } else {
            None
        }
    }

    pub fn finish(&mut self) -> Option<Action> {
        if matches!(self.state, State::Idle | State::Processing) {
            return None;
        }
        self.state = State::Processing;
        Some(Action::Finish)
    }

    pub fn cancel(&mut self) -> Option<Action> {
        if self.state == State::Idle {
            return None;
        }
        self.state = State::Idle;
        self.started = None;
        Some(Action::Cancel)
    }

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
            g.press(t + TAP + DOUBLE_TAP - Duration::from_millis(1)),
            Some(Action::ModeChanged)
        );
        assert_eq!(g.state, State::HandsFree);
        assert_eq!(g.started, Some(t));
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
