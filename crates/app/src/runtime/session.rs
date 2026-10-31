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
use speakeasy_show::{Narration, NarrationLoss};
use tokio::task::{JoinError, JoinHandle};

use super::{
    Phase,
    ports::{Footage, Published, Sealed, SessionId},
    recognition::{Detail, Recognition, Request, Ticket, Transcript},
};
use crate::show::{
    AudioLog, Checkpointing, FrameLog, Kept, Origin, Publication, Reopened, SavedShow,
};

pub(super) struct Session {
    pub id: SessionId,
    pub permit: InsertPermit,
    /// When the session began; a Show's timeline counts from here.
    pub started: Instant,
    stage: Stage,
    recognition: Recognition,
    /// The screen side of a Show; `None` while the session is plain dictation.
    show: Option<Show>,
    receipt: Option<async_channel::Sender<Result<SavedShow, String>>>,
}

struct Show {
    screen: Screen,
    audio: Option<AudioLog>,
    frames: Option<FrameLog>,
    checkpoint: Option<Checkpointing>,
    finished: bool,
    forfeited: bool,
    origin: Origin,
    incomplete: bool,
}

impl Show {
    const fn new(screen: Screen) -> Self {
        Self {
            screen,
            audio: None,
            frames: None,
            checkpoint: None,
            finished: false,
            forfeited: false,
            origin: Origin::Live,
            incomplete: false,
        }
    }
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
    Reopening(JoinHandle<Delivered>),
    /// `audio` is where the first samples fall.
    Recording {
        audio: Duration,
    },
    /// The gesture has finished; waits for the capture's audio.
    Stopping {
        audio: Duration,
    },
    /// Capture sealed; recognition owns its pieces, fallback audio, and request tickets.
    Recognizing,
    /// A Show's narration, placed on its timeline, waits for its screen recording to finish; `lost`
    /// says why it holds nothing the user said, when audio capture or transcription failed.
    AwaitingFootage {
        narration: Narration,
        lost: Option<NarrationLoss>,
    },
    /// Dictated text is being inserted, or a Show composed, written, and copied.
    Delivering(JoinHandle<Delivered>),
}

/// What a session's last task returns: how its dictated text was inserted, or its Show
/// published.
pub(super) enum Delivered {
    Inserted(anyhow::Result<Inserted>),
    Published(anyhow::Result<Published>),
    Abandoned(anyhow::Result<()>),
    Reopened(anyhow::Result<Box<Reopened>>),
}

impl Session {
    pub(super) const fn new(id: SessionId, permit: InsertPermit, started: Instant) -> Self {
        Self {
            id,
            permit,
            started,
            stage: Stage::Queued,
            recognition: Recognition::new(id),
            show: None,
            receipt: None,
        }
    }

    pub(super) fn recovering(
        id: SessionId,
        permit: InsertPermit,
        started: Instant,
        receipt: async_channel::Sender<Result<SavedShow, String>>,
        work: impl Future<Output = anyhow::Result<Reopened>> + Send + 'static,
    ) -> Self {
        let mut session = Self::new(id, permit, started);
        session.show = Some(Show::new(Screen::Spent));
        session.receipt = Some(receipt);
        session.stage = Stage::Reopening(tokio::spawn(async move {
            Delivered::Reopened(work.await.map(Box::new))
        }));
        session
    }

    pub(super) fn recovery_failed(&mut self, message: &str) {
        if let Some(receipt) = self.receipt.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A closed library no longer waits for its failure notice"
            )]
            let _ = receipt.try_send(Err(format!("Cannot recover this Show: {message}")));
        }
    }

    pub(super) fn reopened(&mut self, reopened: Reopened) {
        let Reopened {
            mut footage,
            checkpoint,
            speech,
            pending,
        } = reopened;
        let mut show = Show::new(Screen::Spent);
        show.checkpoint = Some(Checkpointing::Held(checkpoint));
        show.finished = true;
        show.origin = Origin::Recovered { pending };
        self.stage = match speech {
            Some(speech) => {
                show.incomplete = speech.incomplete;
                self.recognition.seal(
                    speech.sealed,
                    speech.start,
                    Some(speech.language),
                    Detail::Words,
                );
                Stage::Recognizing
            },
            None => Stage::AwaitingFootage {
                narration: mem::take(&mut footage.recording.narration),
                lost: footage.recording.narration_lost,
            },
        };
        show.screen = Screen::Recorded(Box::new(footage));
        self.show = Some(show);
    }

    pub(super) fn keep(&mut self, kept: Kept) {
        if let Some(show) = &mut self.show {
            show.checkpoint = Some(Checkpointing::Writing(kept.keeper));
            show.audio = Some(kept.audio);
            show.frames = Some(kept.frames);
        }
    }

    pub(super) fn frames(&mut self) -> Option<FrameLog> {
        self.show.as_mut()?.frames.take()
    }

    pub(super) fn audio(&mut self) -> Option<(AudioLog, Duration)> {
        let (Stage::Recording { audio } | Stage::Stopping { audio }) = self.stage else {
            return None;
        };
        self.show.as_mut()?.audio.take().map(|log| (log, audio))
    }

    pub(super) fn forfeit(&mut self) {
        if let Some(show) = &mut self.show {
            show.forfeited = true;
        }
    }

    pub(super) const fn phase(&self) -> Phase {
        match self.stage {
            Stage::Queued | Stage::Opening => Phase::Starting,
            Stage::Recording { .. } => Phase::Recording,
            Stage::Stopping { .. } => Phase::Stopping,
            Stage::Reopening(_)
            | Stage::Recognizing
            | Stage::AwaitingFootage { .. }
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

    pub(super) fn awaits_worker(&self) -> bool {
        self.is_recognizing() && self.recognition.waiting()
    }

    pub(super) fn is_transcribing(&self) -> bool {
        self.is_recognizing() && self.recognition.in_flight()
    }

    pub(super) const fn is_recognizing(&self) -> bool {
        matches!(self.stage, Stage::Recognizing)
    }

    /// What the Show's screen recorder, queued behind the previous one, is to record.
    pub(super) const fn queued_screen(&self) -> Option<Scope> {
        match &self.show {
            Some(Show {
                screen: Screen::Queued(scope),
                ..
            }) => Some(*scope),
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
        if gesture == State::Processing
            && let Some(show) = &mut self.show
        {
            show.finished = true;
        }
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
        if let Some(show) = &mut self.show {
            show.finished = true;
        }
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
                let detail = if self.is_show() {
                    Detail::Words
                } else {
                    Detail::Text
                };
                self.recognition.seal(sealed, start, None, detail);
                Stage::Recognizing
            },
            _ if self.is_show() => Stage::AwaitingFootage {
                narration: Narration::default(),
                lost: None,
            },
            _ => return false,
        };
        true
    }

    pub(super) fn piece(&mut self, number: u32, audio: Sealed) {
        if self.is_capturing() && !self.is_show() {
            self.recognition.piece(number, audio);
        }
    }

    /// A ready worker may pull a piece during capture, or final audio after capture seals.
    pub(super) fn request(&mut self) -> Option<Request> {
        if self.is_capturing() || self.is_recognizing() {
            self.recognition.request()
        } else {
            None
        }
    }

    pub(super) fn accepts(&self, ticket: Ticket) -> bool {
        (self.is_capturing() || self.is_recognizing()) && self.recognition.accepts(ticket)
    }

    pub(super) fn heard(&mut self, ticket: Ticket, transcript: Transcript) -> Option<String> {
        if !self.accepts(ticket) {
            return None;
        }
        match self.recognition.heard(ticket, transcript)? {
            Transcript::Text(text) => Some(text),
            Transcript::Words(narration) => {
                self.narrate(narration);
                None
            },
        }
    }

    pub(super) fn complete(&mut self) -> Option<String> {
        if self.is_recognizing() {
            self.recognition.complete()
        } else {
            None
        }
    }

    pub(super) fn failed(&mut self, ticket: Ticket) -> bool {
        !self.accepts(ticket) || self.recognition.failed(ticket)
    }

    /// Places a transcribed Show's narration, timed from the start of its WAV, onto the Show's
    /// timeline, then waits for the footage. Any other stage ignores it.
    pub(super) fn narrate(&mut self, narration: Narration) {
        if self.is_recognizing()
            && let Some(start) = self.recognition.start()
        {
            self.recognition = Recognition::new(self.id);
            self.stage = Stage::AwaitingFootage {
                narration: narration.delayed(start),
                lost: self
                    .show
                    .as_ref()
                    .filter(|show| show.incomplete)
                    .map(|_| NarrationLoss::Incomplete),
            };
        }
    }

    /// Sends a Show whose audio capture stopped after it began listening on to its footage without
    /// narration; returns whether it did. A microphone that fails while opening has recorded
    /// nothing worth keeping, so that Show fails with the microphone's remedy, as dictation always
    /// does.
    pub(super) fn audio_stopped(&mut self) -> bool {
        let listened = self.is_show()
            && matches!(self.stage, Stage::Recording { .. } | Stage::Stopping { .. });
        if listened {
            self.go_on_without_narration(NarrationLoss::AudioStopped);
        }
        listened
    }

    /// Sends a Show whose sealed speech could not be transcribed, still waiting for a worker or
    /// failed again on its one retry, on to its footage without narration, dropping the speech;
    /// returns whether it did.
    pub(super) fn not_transcribed(&mut self) -> bool {
        let given_up =
            self.is_show() && self.is_recognizing() && self.recognition.may_lose_narration();
        if given_up {
            self.go_on_without_narration(NarrationLoss::NotTranscribed);
        }
        given_up
    }

    fn go_on_without_narration(&mut self, lost: NarrationLoss) {
        self.recognition = Recognition::new(self.id);
        self.stage = Stage::AwaitingFootage {
            narration: Narration::default(),
            lost: Some(lost),
        };
    }

    /// Delivers transcribed dictation through `insert`, which receives the session's permit.
    pub(super) fn insert<F>(&mut self, insert: impl FnOnce(InsertPermit) -> F)
    where
        F: Future<Output = anyhow::Result<Inserted>> + Send + 'static,
    {
        self.recognition = Recognition::new(self.id);
        let insertion = insert(self.permit.clone());
        self.deliver(async move { Delivered::Inserted(insertion.await) });
    }

    /// Turns a session whose audio is still to come, its microphone queued or capturing, into a
    /// Show of what `scope` names; returns whether it did.
    ///
    /// A Show pastes nothing, and the owner decides which cancels discard it, so its permit is
    /// shielded from the cancels that desktop callbacks revoke on their way to the owner. A permit
    /// already revoked keeps the session dictation, which the revoking cancel or settings change
    /// then discards.
    pub(super) fn turn_into_show(&mut self, scope: Scope) -> bool {
        let turns =
            !self.is_show() && (self.is_queued() || self.is_capturing()) && self.permit.shield();
        if turns {
            self.recognition.whole();
            self.show = Some(Show::new(Screen::Queued(scope)));
        }
        turns
    }

    /// The Show's queued screen recorder opened.
    pub(super) fn screen_opened(&mut self) {
        if let Some(Show {
            screen: screen @ Screen::Queued(_),
            ..
        }) = &mut self.show
        {
            *screen = Screen::Recording;
        }
    }

    /// Keeps a Show's footage until its narration is in.
    pub(super) fn screen_recorded(&mut self, footage: Box<Footage>) {
        if let Some(show) = &mut self.show {
            show.screen = Screen::Recorded(footage);
        }
    }

    /// Publishes a Show through `publish` once its narration, or why it was lost, and its footage
    /// are both in; its screen is then spent.
    pub(super) fn publish<F>(&mut self, publish: impl FnOnce(Publication, InsertPermit) -> F)
    where
        F: Future<Output = anyhow::Result<Published>> + Send + 'static,
    {
        let (Stage::AwaitingFootage { narration, lost }, Some(show)) =
            (&mut self.stage, &mut self.show)
        else {
            return;
        };
        if !matches!(show.screen, Screen::Recorded(_)) {
            return;
        }
        let Screen::Recorded(mut footage) = mem::replace(&mut show.screen, Screen::Spent) else {
            return;
        };
        footage.recording.narration = mem::take(narration);
        footage.recording.narration_lost = *lost;
        // A capture may seal before its Ready event reaches the owner. Unhanded producers must
        // close before publication waits for the writer.
        show.audio = None;
        show.frames = None;
        let publication = publish(
            Publication {
                footage: *footage,
                checkpoint: show.checkpoint.take(),
                origin: show.origin,
            },
            self.permit.clone(),
        );
        let receipt = self.receipt.take();
        self.deliver(async move {
            let result = publication.await;
            if let Some(receipt) = receipt {
                let completed = match &result {
                    Ok(Published::Recovered { show, .. } | Published::Saved { show, .. }) => {
                        Ok(show.clone())
                    },
                    Ok(Published::Cancelled) => {
                        Err("Recovery cancelled. The interrupted Show is still available.".into())
                    },
                    Err(error) => Err(format!("Cannot recover this Show: {error:#}")),
                };
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Closing the library abandons its receipt, not the owner's recovery"
                )]
                let _ = receipt.try_send(completed);
            }
            Delivered::Published(result)
        });
    }

    fn deliver(&mut self, delivery: impl Future<Output = Delivered> + Send + 'static) {
        self.stage = Stage::Delivering(tokio::spawn(delivery));
    }

    /// Resolves once the session's insertion or publication finishes, and pends otherwise.
    /// Cancel-safe: dropped early, it leaves the task running.
    pub(super) async fn delivered(&mut self) -> Result<Delivered, JoinError> {
        match &mut self.stage {
            Stage::Delivering(task) | Stage::Reopening(task) => task.await,
            _ => pending().await,
        }
    }

    /// The insertion or publication still under way, which an abandoned session leaves to finish
    /// its own cleanup.
    pub(super) fn into_cleanup(self) -> Option<JoinHandle<Delivered>> {
        if let Stage::Delivering(task) | Stage::Reopening(task) = self.stage {
            return Some(task);
        }
        let show = self.show?;
        let keep = !show.forfeited
            && (show.finished
                || !matches!(
                    self.stage,
                    Stage::Queued
                        | Stage::Opening
                        | Stage::Recording { .. }
                        | Stage::Stopping { .. }
                ));
        let narration = match self.stage {
            Stage::AwaitingFootage {
                narration,
                lost: lost @ (None | Some(NarrationLoss::Incomplete)),
            } => Some((narration, lost.is_some())),
            _ => None,
        };
        drop(show.audio);
        drop(show.frames);
        let checkpoint = show.checkpoint?;
        Some(tokio::spawn(async move {
            Delivered::Abandoned(checkpoint.release(keep, narration).await)
        }))
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
            partition: None,
        };
        assert!(session.captured(Some(sealed)));
        assert!(session.request().is_some());
        let word = |at| Word {
            span: Span::new(Moment::from_millis(at), Moment::from_millis(at + 200)),
            text: "here".into(),
        };
        session.narrate(Narration::Words(vec![word(300)]));
        let Stage::AwaitingFootage { narration, .. } = session.stage else {
            panic!("Narration did not move the Show on to its footage");
        };
        assert_eq!(narration, Narration::Words(vec![word(2_000)]));
    }

    #[test]
    fn a_show_goes_on_without_narration_only_from_the_stages_its_loss_can_reach() {
        let (sender, _inputs) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let opened = |show| {
            let mut session =
                Session::new(SessionId::FIRST, input.begin().unwrap(), Instant::now());
            if show {
                assert!(session.turn_into_show(Scope::Screen));
            }
            session.opened();
            session
        };
        let lost = |session: &Session| match session.stage {
            Stage::AwaitingFootage { lost, .. } => lost,
            _ => None,
        };
        let mut dictation = opened(false);
        dictation.audio_ready(State::Held, Instant::now());
        assert!(!dictation.audio_stopped(), "Dictation has nothing left");

        let mut show = opened(true);
        assert!(
            !show.audio_stopped(),
            "A microphone that never listened fails with its remedy"
        );
        show.audio_ready(State::Held, Instant::now());
        assert!(
            !show.not_transcribed(),
            "A Show still recording has no speech to transcribe"
        );
        show.finish();
        assert!(
            show.audio_stopped(),
            "Audio can still stop while the capture seals it"
        );
        assert_eq!(lost(&show), Some(NarrationLoss::AudioStopped));

        let mut show = opened(true);
        show.audio_ready(State::Held, Instant::now());
        show.finish();
        assert!(show.captured(Some(Sealed::default())));
        assert!(!show.audio_stopped(), "Sealed audio can no longer stop");
        assert!(show.request().is_some());
        assert!(!show.not_transcribed(), "Speech keeps its one retry");
        assert!(show.failed(Ticket {
            session: show.id,
            part: super::super::recognition::Part::Final
        }));
        assert!(show.request().is_some());
        assert!(show.not_transcribed());
        assert_eq!(lost(&show), Some(NarrationLoss::NotTranscribed));
    }
}
