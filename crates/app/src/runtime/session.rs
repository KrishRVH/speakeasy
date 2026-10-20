//! Authority and presentation stage for one recording. Retired native work belongs to the owner, so
//! abandoning a session never waits or opens its replacement.

use std::{
    future::pending,
    time::{Duration, Instant},
};

use speakeasy_core::gesture::State;
use speakeasy_platform::{InsertPermit, Inserted};
use speakeasy_show::Narration;
use tokio::task::{JoinError, JoinHandle};

use super::{
    Phase,
    ports::{Footage, Published, Recorded, SessionId},
};

pub(super) type InsertTask = JoinHandle<anyhow::Result<Inserted>>;
pub(super) type PublishTask = JoinHandle<anyhow::Result<Published>>;

pub(super) struct Session {
    pub id: SessionId,
    pub permit: InsertPermit,
    /// When the session began; a Show's timeline counts from here.
    pub started: Instant,
    /// When the microphone delivered its first audio.
    pub audio_started: Option<Instant>,
    pub stage: Stage,
    pub show: Option<Show>,
}

/// A Show's state beyond its microphone's: what its screen recording and narration produced.
#[derive(Default)]
pub(super) struct Show {
    /// Whether its screen recorder has opened, which waits for the previous one to retire.
    pub opened: bool,
    /// Quiet the microphone trimmed before its audio began.
    pub trimmed: Duration,
    pub footage: Option<Footage>,
    /// The narration on the Show's timeline; empty when nothing was said.
    pub narration: Option<Narration>,
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
            | Stage::Transcribing
            | Stage::AwaitingFootage
            | Stage::Inserting(_)
            | Stage::Publishing(_) => Phase::Processing,
        }
    }

    /// Places narration timed from the start of its audio onto the Show's timeline.
    pub(super) fn narrate(&mut self, narration: Narration) {
        let audio = self.audio_started.map_or(Duration::ZERO, |audio| {
            audio.saturating_duration_since(self.started)
        });
        if let Some(show) = &mut self.show {
            show.narration = Some(narration.delayed(audio.saturating_add(show.trimmed)));
        }
    }

    /// The finished Show, once both its narration and its screen recording are in hand.
    pub(super) fn take_recorded(&mut self) -> Option<Recorded> {
        let show = self.show.as_mut()?;
        if show.narration.is_none() || show.footage.is_none() {
            return None;
        }
        let (narration, footage) = (show.narration.take()?, show.footage.take()?);
        Some(Recorded {
            recording: speakeasy_show::Recording {
                duration: footage.ended,
                surface: footage.surface,
                strokes: footage.strokes,
                actions: footage.actions,
                narration,
            },
            journal: footage.journal,
        })
    }

    pub(super) const fn is_capturing(&self) -> bool {
        matches!(
            self.stage,
            Stage::Opening | Stage::Recording | Stage::Stopping
        )
    }

    /// Resolves once the session's insertion or publication finishes, and pends otherwise.
    /// Cancel-safe: dropped early, it leaves the task running.
    pub(super) async fn delivered(&mut self) -> Delivered {
        match &mut self.stage {
            Stage::Inserting(task) => Delivered::Insertion(task.await),
            Stage::Publishing(task) => Delivered::Publication(task.await),
            _ => pending().await,
        }
    }

    /// Starts recording once samples arrive, or stops at once when the gesture already finished.
    pub(super) fn audio_ready(&mut self, gesture: State) {
        if matches!(self.stage, Stage::Opening) {
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

/// How a session's last task ended: dictated text reached the editor, or a Show was published.
pub(super) enum Delivered {
    Insertion(Result<anyhow::Result<Inserted>, JoinError>),
    Publication(Result<anyhow::Result<Published>, JoinError>),
}

pub(super) enum Stage {
    /// Waits until any previous capture has released the microphone.
    Queued,
    Opening,
    Recording,
    /// The gesture has finished; waits for the capture's audio.
    Stopping,
    /// Captured WAV awaiting a ready worker; zeroed, best effort, if the session is abandoned.
    AwaitingWorker(Vec<u8>),
    Transcribing,
    /// A Show's narration is ready; waits for its screen recording to finish.
    AwaitingFootage,
    Inserting(InsertTask),
    /// A Show is being composed, written, and copied.
    Publishing(PublishTask),
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
        session.audio_started = started.checked_add(Duration::from_millis(1_200));
        session.show = Some(Show {
            trimmed: Duration::from_millis(500),
            ..Show::default()
        });
        let word = |at| Word {
            span: Span::new(Moment::from_millis(at), Moment::from_millis(at + 200)),
            text: "here".into(),
        };
        session.narrate(Narration::Words(vec![word(300)]));
        let narration = session.show.and_then(|show| show.narration);
        assert_eq!(narration, Some(Narration::Words(vec![word(2_000)])));
    }
}
