//! Authority and stage for one recording: what each stage holds and how one stage becomes the
//! next, while the owner decides when. Retired native work belongs to the owner, so abandoning a
//! session never waits or opens its replacement.

use std::{
    future::pending,
    mem,
    time::{Duration, Instant},
};

use speakeasy_core::gesture::State;
use speakeasy_platform::{InsertPermit, Inserted, Scope};
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
    stage: Stage,
    /// The screen side of a Show; `None` while the session is plain dictation.
    show: Option<Screen>,
}

/// A Show's screen recorder, which opens only after the previous one has retired.
enum Screen {
    /// Waits to record what the scope names.
    Queued(Scope),
    Recording,
    /// Boxed, since footage is far larger than the other states.
    Recorded(Box<Footage>),
    /// The footage has moved into the publication.
    Spent,
}

/// Each offset places audio on the session's timeline, so a Show's narration lands where it was
/// spoken.
enum Stage {
    /// Waits until any previous capture has released the microphone.
    Queued,
    Opening,
    /// `audio` is where the first samples fall.
    Recording {
        audio: Duration,
    },
    /// The gesture has finished; waits for the capture's audio.
    Stopping {
        audio: Duration,
    },
    /// Sealed speech, starting at `start`, awaits a ready worker.
    AwaitingWorker {
        sealed: Sealed,
        start: Duration,
    },
    /// The worker holds the WAV, which starts at `start`.
    Transcribing {
        start: Duration,
    },
    /// A Show's narration, placed on its timeline, waits for its screen recording to finish.
    AwaitingFootage(Narration),
    /// Dictated text is being inserted, or a Show composed, written, and copied.
    Delivering(JoinHandle<Delivered>),
}

/// What a session's last task returns: how its dictated text was inserted, or its Show
/// published.
pub(super) enum Delivered {
    Inserted(anyhow::Result<Inserted>),
    Published(anyhow::Result<Published>),
}

impl Session {
    pub(super) const fn new(id: SessionId, permit: InsertPermit, started: Instant) -> Self {
        Self {
            id,
            permit,
            started,
            stage: Stage::Queued,
            show: None,
        }
    }

    pub(super) const fn phase(&self) -> Phase {
        match self.stage {
            Stage::Queued | Stage::Opening => Phase::Starting,
            Stage::Recording { .. } => Phase::Recording,
            Stage::Stopping { .. } => Phase::Stopping,
            Stage::AwaitingWorker { .. }
            | Stage::Transcribing { .. }
            | Stage::AwaitingFootage(_)
            | Stage::Delivering(_) => Phase::Processing,
        }
    }

    /// Whether the session became a Show, recording the screen as well as the microphone.
    pub(super) const fn is_show(&self) -> bool {
        self.show.is_some()
    }

    /// Whether the session's microphone waits for the previous one to retire.
    pub(super) const fn is_queued(&self) -> bool {
        matches!(self.stage, Stage::Queued)
    }

    pub(super) const fn is_recording(&self) -> bool {
        matches!(self.stage, Stage::Recording { .. })
    }

    pub(super) const fn is_capturing(&self) -> bool {
        matches!(
            self.stage,
            Stage::Opening | Stage::Recording { .. } | Stage::Stopping { .. }
        )
    }

    pub(super) const fn awaits_worker(&self) -> bool {
        matches!(self.stage, Stage::AwaitingWorker { .. })
    }

    pub(super) const fn is_transcribing(&self) -> bool {
        matches!(self.stage, Stage::Transcribing { .. })
    }

    /// What the Show's screen recorder, queued behind the previous one, is to record.
    pub(super) const fn queued_screen(&self) -> Option<Scope> {
        match self.show {
            Some(Screen::Queued(scope)) => Some(scope),
            _ => None,
        }
    }

    /// The microphone opened; the session waits for its first samples.
    pub(super) fn opened(&mut self) {
        if self.is_queued() {
            self.stage = Stage::Opening;
        }
    }

    /// Notes where the first samples fall and starts recording, or stops at once when the gesture
    /// already finished.
    pub(super) fn audio_ready(&mut self, gesture: State, at: Instant) {
        if matches!(self.stage, Stage::Opening) {
            let audio = at.saturating_duration_since(self.started);
            self.stage = if gesture == State::Processing {
                Stage::Stopping { audio }
            } else {
                Stage::Recording { audio }
            };
        }
    }

    pub(super) fn finish(&mut self) {
        if let Stage::Recording { audio } = self.stage {
            self.stage = Stage::Stopping { audio };
        }
    }

    /// Ends capture with what it sealed, returning whether anything is left to deliver. Speech
    /// goes on to transcription. A silent Show goes on to its footage, since its ink and clicks
    /// still tell; silent dictation has nothing. Audio sealed by a capture that never reported its
    /// first samples has no place on the timeline, so it counts as silence.
    pub(super) fn captured(&mut self, sealed: Option<Sealed>) -> bool {
        self.stage = match (&self.stage, sealed) {
            (Stage::Recording { audio } | Stage::Stopping { audio }, Some(sealed)) => {
                let start = audio.saturating_add(sealed.trimmed);
                Stage::AwaitingWorker { sealed, start }
            },
            _ if self.is_show() => Stage::AwaitingFootage(Narration::default()),
            _ => return false,
        };
        true
    }

    /// Hands the sealed WAV to `transcribe`, which gives it back while no worker can take it.
    pub(super) fn transcribe(&mut self, transcribe: impl FnOnce(Vec<u8>) -> Result<(), Vec<u8>>) {
        let Stage::AwaitingWorker { sealed, start } = &mut self.stage else {
            return;
        };
        let start = *start;
        match transcribe(mem::take(&mut sealed.wav)) {
            Ok(()) => self.stage = Stage::Transcribing { start },
            Err(unclaimed) => sealed.wav = unclaimed,
        }
    }

    /// Places a transcribed Show's narration, timed from the start of its WAV, onto the Show's
    /// timeline, then waits for the footage. Any other stage ignores it.
    pub(super) fn narrate(&mut self, narration: Narration) {
        if let Stage::Transcribing { start } = self.stage {
            self.stage = Stage::AwaitingFootage(narration.delayed(start));
        }
    }

    /// Delivers transcribed dictation through `insert`, which receives the session's permit.
    pub(super) fn insert<F>(&mut self, insert: impl FnOnce(InsertPermit) -> F)
    where
        F: Future<Output = anyhow::Result<Inserted>> + Send + 'static,
    {
        let insertion = insert(self.permit.clone());
        self.deliver(async move { Delivered::Inserted(insertion.await) });
    }

    /// Turns a session whose audio is still to come, its microphone queued or capturing, into a
    /// Show of what `scope` names; returns whether it did.
    pub(super) fn turn_into_show(&mut self, scope: Scope) -> bool {
        let turns = !self.is_show() && (self.is_queued() || self.is_capturing());
        if turns {
            self.show = Some(Screen::Queued(scope));
        }
        turns
    }

    /// The Show's queued screen recorder opened.
    pub(super) fn screen_opened(&mut self) {
        if let Some(screen @ Screen::Queued(_)) = &mut self.show {
            *screen = Screen::Recording;
        }
    }

    /// Keeps a Show's footage until its narration is in.
    pub(super) fn screen_recorded(&mut self, footage: Footage) {
        if let Some(screen) = &mut self.show {
            *screen = Screen::Recorded(Box::new(footage));
        }
    }

    /// Publishes a Show through `publish` once its narration and footage are both in; its screen
    /// is then spent.
    pub(super) fn publish<F>(&mut self, publish: impl FnOnce(Footage, Narration, InsertPermit) -> F)
    where
        F: Future<Output = anyhow::Result<Published>> + Send + 'static,
    {
        let (Stage::AwaitingFootage(narration), Some(screen @ Screen::Recorded(_))) =
            (&mut self.stage, &mut self.show)
        else {
            return;
        };
        let Screen::Recorded(footage) = mem::replace(screen, Screen::Spent) else {
            return;
        };
        let publication = publish(*footage, mem::take(narration), self.permit.clone());
        self.deliver(async move { Delivered::Published(publication.await) });
    }

    fn deliver(&mut self, delivery: impl Future<Output = Delivered> + Send + 'static) {
        self.stage = Stage::Delivering(tokio::spawn(delivery));
    }

    /// Resolves once the session's insertion or publication finishes, and pends otherwise.
    /// Cancel-safe: dropped early, it leaves the task running.
    pub(super) async fn delivered(&mut self) -> Result<Delivered, JoinError> {
        match &mut self.stage {
            Stage::Delivering(task) => task.await,
            _ => pending().await,
        }
    }

    /// The insertion or publication still under way, which an abandoned session leaves to finish
    /// its own cleanup.
    pub(super) fn into_delivery(self) -> Option<JoinHandle<Delivered>> {
        match self.stage {
            Stage::Delivering(task) => Some(task),
            _ => None,
        }
    }
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
        assert!(session.turn_into_show(Scope::Screen));
        session.opened();
        let first_audio = started.checked_add(Duration::from_millis(1_200)).unwrap();
        session.audio_ready(State::Held, first_audio);
        session.finish();
        let sealed = Sealed {
            wav: vec![1, 2],
            trimmed: Duration::from_millis(500),
        };
        assert!(session.captured(Some(sealed)));
        session.transcribe(|_| Ok(()));
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
