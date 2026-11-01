use std::{
    mem,
    time::{Duration, Instant},
};

pub const RECORDING_LIMIT: Duration = Duration::from_secs(300);

const TAP: Duration = Duration::from_millis(220);

const DOUBLE_TAP: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
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
}

#[derive(Debug, Default)]
pub struct Gesture {
    phase: Phase,
    down: bool,
}

impl Gesture {
    pub fn deliver(&mut self) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.phase = Phase::Processing;
        true
    }

    pub fn save(&mut self) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.phase = Phase::Saving;
        true
    }

    #[must_use]
    pub fn state(&self) -> State {
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

    pub fn release(&mut self, now: Instant) -> Option<Action> {
        let expired = self.tick(now);
        self.down = false;
        match self.phase {
            Phase::Held { started } if now.duration_since(started) <= TAP => {
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

    pub fn lock(&mut self) {
        if let Phase::Held { started } = self.phase {
            self.phase = Phase::HandsFree { started };
        }
    }

    pub fn show(&mut self) {
        if let Phase::Held { started } | Phase::HandsFree { started } = self.phase {
            self.phase = Phase::Show { started };
        }
    }

    pub fn toggle(&mut self, now: Instant) -> Option<Action> {
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
    pub fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Idle | Phase::Processing | Phase::Saving => None,
            Phase::Held { started }
            | Phase::HandsFree { started }
            | Phase::Show { started }
            | Phase::ShowArmed { started } => Some(expiry(started, RECORDING_LIMIT)),
            Phase::PendingTap { started, released } => {
                Some(expiry(started, RECORDING_LIMIT).min(expiry(released, DOUBLE_TAP)))
            },
        }
    }

    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.finish()
        } else {
            None
        }
    }

    pub fn finish(&mut self) -> Option<Action> {
        self.phase = match self.phase {
            Phase::Idle | Phase::Processing | Phase::Saving => return None,
            Phase::Held { .. } | Phase::PendingTap { .. } | Phase::HandsFree { .. } => {
                Phase::Processing
            },
            Phase::Show { .. } | Phase::ShowArmed { .. } => Phase::Saving,
        };
        Some(Action::Finish)
    }

    pub fn escape(&mut self) -> Option<Action> {
        match self.phase {
            Phase::Show { .. } | Phase::ShowArmed { .. } if !self.down => None,
            _ => self.cancel(),
        }
    }

    pub fn interrupt(&mut self) -> Option<Action> {
        match self.phase {
            Phase::Show { .. } | Phase::ShowArmed { .. } => self.finish(),
            _ => self.cancel(),
        }
    }

    pub fn cancel(&mut self) -> Option<Action> {
        if self.phase == Phase::Saving {
            return None;
        }
        self.abandon()
    }

    pub fn abandon(&mut self) -> Option<Action> {
        if self.phase == Phase::Idle {
            return None;
        }
        self.phase = Phase::Idle;
        Some(Action::Cancel)
    }

    pub fn complete(&mut self) {
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
        started: Instant,
    },
    PendingTap {
        started: Instant,
        released: Instant,
    },
    HandsFree {
        started: Instant,
    },
    Show {
        started: Instant,
    },
    ShowArmed {
        started: Instant,
    },
    Processing,
    Saving,
}

fn expiry(start: Instant, after: Duration) -> Instant {
    start.checked_add(after).unwrap_or(start)
}

#[cfg(test)]
mod tests;
