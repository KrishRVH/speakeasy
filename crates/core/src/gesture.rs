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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// No recording or transcription is active.
    Idle,
    /// Recording while the shortcut is held.
    Held,
    /// A short release is waiting for a possible second tap.
    PendingTap,
    /// Recording until the shortcut finishes it, cancellation, or the deadline.
    HandsFree,
    /// Capture is complete; transcription, insertion, or saving a Show is pending.
    Processing,
}

/// Work requested by a gesture transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Opens a new recording.
    Start,
    /// Stops capture and transcribes its audio.
    Finish,
    /// Discards the recording and revokes its insertion authority.
    Cancel,
}

/// Hold-to-talk, double-tap hands-free, Space locking, and Shows for one shortcut.
///
/// Hands-free dictation finishes on the next press, a Show on that press's release, so Escape can
/// cancel a Show while the shortcut is held; Escape alone leaves a Show recording, and an
/// interruption finishes it. No request to cancel discards a Show once it is being saved: only
/// [`abandon`](Self::abandon) does.
///
/// Physical key state survives completion and cancellation, preventing autorepeat from starting a
/// new session before the user releases the shortcut.
#[derive(Debug, Default)]
pub struct Gesture {
    phase: Phase,
    down: bool,
}

impl Gesture {
    /// Starts an explicit saved-Show recovery while idle. Like publication, it ignores passive
    /// Escape and cancel gestures; settings changes and shutdown still abandon it.
    pub fn save(&mut self) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.phase = Phase::Saving;
        true
    }

    /// The current mode; only gesture transitions change it.
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

    /// Observes a physical press; autorepeat cannot open another recording. A press finishes
    /// hands-free dictation and arms a Show to finish on its release.
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

    /// Observes a release, finishing a hold or an armed Show, or waiting for a second tap.
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

    /// Switches a hold to hands-free, keeping its capture, as Space does.
    pub fn lock(&mut self) {
        if let Phase::Held { started } = self.phase {
            self.phase = Phase::HandsFree { started };
        }
    }

    /// Turns a held or hands-free recording into a Show, keeping its capture. A Show records
    /// hands-free, so releasing the shortcut that started it does not finish it.
    pub fn show(&mut self) {
        if let Phase::Held { started } | Phase::HandsFree { started } = self.phase {
            self.phase = Phase::Show { started };
        }
    }

    /// Starts hands-free recording or finishes the active recording.
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

    /// The earliest tap or recording deadline; callers wake and tick at this instant.
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

    /// Finishes an expired recording or tap without consulting an ambient clock.
    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.finish()
        } else {
            None
        }
    }

    /// Moves active capture into processing, or a Show into saving, without changing physical key
    /// state.
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

    /// Observes Escape, which also reaches the focused app: it cancels dictation at any stage, but
    /// a Show only while the shortcut is held, and never once it is being saved.
    pub fn escape(&mut self) -> Option<Action> {
        match self.phase {
            Phase::Show { .. } | Phase::ShowArmed { .. } if !self.down => None,
            _ => self.cancel(),
        }
    }

    /// Observes an interruption the user never asked for, such as another shortcut or a session
    /// change: it discards dictation at any stage but finishes a recording Show, and leaves a Show
    /// being saved alone.
    pub fn interrupt(&mut self) -> Option<Action> {
        match self.phase {
            Phase::Show { .. } | Phase::ShowArmed { .. } => self.finish(),
            _ => self.cancel(),
        }
    }

    /// Cancels at the user's request: anything but a Show being saved.
    pub fn cancel(&mut self) -> Option<Action> {
        if self.phase == Phase::Saving {
            return None;
        }
        self.abandon()
    }

    /// Abandons capture or processing, a Show being saved included, as a failure or new settings
    /// do; a held shortcut still requires release.
    pub fn abandon(&mut self) -> Option<Action> {
        if self.phase == Phase::Idle {
            return None;
        }
        self.phase = Phase::Idle;
        Some(Action::Cancel)
    }

    /// Acknowledges that processing or saving completed; an active recording is unaffected.
    pub fn complete(&mut self) {
        if matches!(self.phase, Phase::Processing | Phase::Saving) {
            self.phase = Phase::Idle;
        }
    }
}

/// A [`State`] with the instants its deadlines count from, telling a Show from dictation.
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
    /// A Show recording hands-free; the shortcut's next press arms its finish.
    Show {
        started: Instant,
    },
    /// The shortcut went down again during a Show, whose release finishes it.
    ShowArmed {
        started: Instant,
    },
    Processing,
    /// A finished Show waits for its narration and footage and is saved.
    Saving,
}

/// `start + after`, or `start` itself when that instant is unrepresentable, so an overflowing
/// deadline expires immediately rather than removing the cap.
fn expiry(start: Instant, after: Duration) -> Instant {
    start.checked_add(after).unwrap_or(start)
}

#[cfg(test)]
mod tests;
