//! Authority and presentation stage for one recording. Retired native work belongs to the owner, so
//! abandoning a session never waits or opens its replacement.

use std::{
    future::pending,
    mem,
    time::{Duration, Instant},
};

use speakeasy_core::gesture::State;
use speakeasy_platform::{InsertPermit, Inserted};
use speakeasy_show::Narration;
use tokio::task::{JoinError, JoinHandle};

use super::{
    Phase,
    ports::{Footage, Published, Sealed, SessionId},
};

pub(super) struct Session {
    pub id: SessionId,
    pub permit: InsertPermit,
    /// When the session began; a Show's timeline counts from here.
    pub started: Instant,
    /// When the microphone delivered its first audio.
    audio_started: Option<Instant>,
    pub stage: Stage,
    /// The screen side of a Show; `None` while the session is plain dictation.
    pub show: Option<Screen>,
}

/// A Show's screen recorder, which opens only after the previous one has retired.
pub(super) enum Screen {
    Queued,
    Recording,
    Recorded(Footage),
    /// The footage has moved into the publication.
    Spent,
}

impl Session {
    pub(super) const fn new(id: SessionId, permit: InsertPermit, started: Instant) -> Self {
        Self {
            id,
            permit,
            started,
            audio_started: None,
            stage: Stage::Queued,
            show: None,
        }
    }

    pub(super) const fn phase(&self) -> Phase {
        match self.stage {
            Stage::Queued | Stage::Opening => Phase::Starting,
            Stage::Recording => Phase::Recording,
            Stage::Stopping => Phase::Stopping,
            Stage::AwaitingWorker(_)
            | Stage::Transcribing { .. }
            | Stage::AwaitingFootage(_)
            | Stage::Delivering(_) => Phase::Processing,
        }
    }

    /// Places a transcribed Show's narration, timed from the start of its audio, onto the Show's
    /// timeline, then waits for the footage. Any other stage ignores it.
    pub(super) fn narrate(&mut self, narration: Narration) {
        let Stage::Transcribing { trimmed } = self.stage else {
            return;
        };
        let audio = self.audio_started.map_or(Duration::ZERO, |audio| {
            audio.saturating_duration_since(self.started)
        });
        self.stage = Stage::AwaitingFootage(narration.delayed(audio.saturating_add(trimmed)));
    }

    /// The finished Show's footage and narration, once both are in hand; its screen is then
    /// spent.
    pub(super) fn take_publishable(&mut self) -> Option<(Footage, Narration)> {
        let (Stage::AwaitingFootage(narration), Some(screen @ Screen::Recorded(_))) =
            (&mut self.stage, self.show.as_mut())
        else {
            return None;
        };
        let Screen::Recorded(footage) = mem::replace(screen, Screen::Spent) else {
            return None;
        };
        Some((footage, mem::take(narration)))
    }

    pub(super) const fn is_capturing(&self) -> bool {
        matches!(
            self.stage,
            Stage::Opening | Stage::Recording | Stage::Stopping
        )
    }

    /// Whether the session's audio is still to come: its microphone is queued behind the previous
    /// one, or capturing.
    pub(super) const fn awaits_audio(&self) -> bool {
        matches!(self.stage, Stage::Queued) || self.is_capturing()
    }

    /// Resolves once the session's insertion or publication finishes, and pends otherwise.
    /// Cancel-safe: dropped early, it leaves the task running.
    pub(super) async fn delivered(&mut self) -> Result<Delivered, JoinError> {
        match &mut self.stage {
            Stage::Delivering(task) => task.await,
            _ => pending().await,
        }
    }

    /// Notes when the first samples arrived and starts recording, or stops at once when the gesture
    /// already finished.
    pub(super) fn audio_ready(&mut self, gesture: State, at: Instant) {
        if matches!(self.stage, Stage::Opening) {
            self.audio_started = Some(at);
            self.stage = if gesture == State::Processing {
                Stage::Stopping
            } else {
                Stage::Recording
            };
        }
    }

    pub(super) fn finish(&mut self) {
        if matches!(self.stage, Stage::Recording) {
            self.stage = Stage::Stopping;
        }
    }
}

/// What a session's last task returns: how its dictated text was inserted, or its Show
/// published.
pub(super) enum Delivered {
    Inserted(anyhow::Result<Inserted>),
    Published(anyhow::Result<Published>),
}

pub(super) enum Stage {
    /// Waits until any previous capture has released the microphone.
    Queued,
    Opening,
    Recording,
    /// The gesture has finished; waits for the capture's audio.
    Stopping,
    /// Captured audio awaiting a ready worker; its WAV is zeroed, best effort, if the session is
    /// abandoned.
    AwaitingWorker(Sealed),
    /// The worker holds the WAV; `trimmed` places the times it returns on the recording's clock.
    Transcribing {
        trimmed: Duration,
    },
    /// A Show's narration, placed on its timeline, waits for its screen recording to finish.
    AwaitingFootage(Narration),
    /// Dictated text is being inserted, or a Show composed, written, and copied.
    Delivering(JoinHandle<Delivered>),
}

#[cfg(test)]
mod tests {
    use speakeasy_platform::InputSender;
    use speakeasy_show::{Moment, Span, Word};

    use super::*;

    #[test]
    fn narration_lands_where_it_was_spoken_on_the_show_timeline() {
        let (sender, _inputs) = async_channel::bounded(1);
        let permit = InputSender::new(sender).begin().unwrap();
        let started = Instant::now();
        let mut session = Session::new(SessionId::FIRST, permit, started);
        session.show = Some(Screen::Recording);
        session.stage = Stage::Opening;
        let first_audio = started.checked_add(Duration::from_millis(1_200)).unwrap();
        session.audio_ready(State::Held, first_audio);
        session.stage = Stage::Transcribing {
            trimmed: Duration::from_millis(500),
        };
        let word = |at| Word {
            span: Span::new(Moment::from_millis(at), Moment::from_millis(at + 200)),
            text: "here".into(),
        };
        session.narrate(Narration::Words(vec![word(300)]));
        let Stage::AwaitingFootage(narration) = session.stage else {
            panic!("Narration did not move the Show on to its footage");
        };
        assert_eq!(narration, Narration::Words(vec![word(2_000)]));
    }
}
