//! The single session authority. Each wake handles one message, advances work whose prerequisites
//! are satisfied, then publishes a projection of owned state.

use std::{convert::Infallible, mem, ops::ControlFlow, sync::Arc, time::Instant};

use async_channel::{Receiver, RecvError, Sender};
use speakeasy_core::gesture::{Action, Gesture, State};
use speakeasy_platform::{Input, InputSender, Inserted};
use tokio::{sync::watch, task::JoinError};

use super::{
    CaptureEvent, Captured, LOADING, Phase, Snapshot, Wiring,
    microphone::Microphone,
    now, reap,
    session::{InsertTask, Session, SessionId, Stage},
    timeline::Timeline,
    worker::{Completion, Transcript, Worker},
};
use crate::{
    audio,
    config::Config,
    ports::{Ports, Recording},
};

/// What the status shows while no session is active: the latest result or failure.
enum Outcome {
    Idle,
    Done,
    Empty,
    Cancelled,
    Failed(String),
}

impl Outcome {
    /// How the timing report names the way a session ended.
    const fn label(&self) -> &'static str {
        match self {
            Self::Idle => "abandoned",
            Self::Done => "done",
            Self::Empty => "no speech",
            Self::Cancelled => "cancelled",
            Self::Failed(_) => "failed",
        }
    }

    const fn phase(&self) -> Phase {
        match self {
            Self::Idle => Phase::Idle,
            Self::Done => Phase::Done,
            Self::Empty => Phase::Empty,
            Self::Cancelled => Phase::Cancelled,
            Self::Failed(_) => Phase::Error,
        }
    }
}

/// Status text shown unless a failure is being reported.
enum Notice {
    LoadingModel,
    SettingsApplied,
    WarmupFailed(String),
}

impl Notice {
    fn text(&self) -> &str {
        match self {
            Self::LoadingModel => LOADING,
            Self::SettingsApplied => "Settings applied",
            Self::WarmupFailed(message) => message,
        }
    }
}

enum Wake<W> {
    Stop,
    Input(Result<Input, RecvError>),
    Configuration(Result<(), watch::error::RecvError>),
    Capture(Result<CaptureEvent, RecvError>),
    Worker(Completion<W>),
    Insertion(Result<anyhow::Result<Inserted>, JoinError>),
    Retired,
    Timer,
}

struct Owner<P: Ports> {
    ports: P,
    config: Config,

    changes: watch::Receiver<Config>,
    inputs: Receiver<Input>,
    stopping: Receiver<Infallible>,
    events: Sender<CaptureEvent>,
    captures: Receiver<CaptureEvent>,

    permits: InputSender,
    gesture: Gesture,
    session: Option<Session>,
    /// The latest ended session's stages, reported once its outcome is known.
    ended: Option<(Timeline, Instant)>,
    latest_id: SessionId,
    latest_started: Instant,

    worker: Worker<P::Speech>,
    microphone: Microphone<P::Recording>,
    retiring_insertions: Vec<InsertTask>,

    snapshots: watch::Sender<Snapshot>,
    epoch: u64,
    outcome: Outcome,
    notice: Option<Notice>,
    desktop_ready: bool,
    shortcut: Arc<str>,
    cancel_shortcut: Arc<str>,
}

impl<P: Ports> Owner<P> {
    fn new(wiring: Wiring<P>) -> Self {
        let Wiring {
            mut changes,
            inputs,
            snapshots,
            permits,
            stopping,
            ports,
            epoch,
        } = wiring;
        let config = changes.borrow_and_update().clone();
        let (events, captures) = async_channel::bounded(64);
        let desktop_ready = !ports.prepares_desktop();
        let worker = Worker::load(&ports, &config);
        Self {
            ports,
            config,
            changes,
            inputs,
            stopping,
            events,
            captures,
            permits,
            gesture: Gesture::default(),
            session: None,
            ended: None,
            latest_id: SessionId::default(),
            latest_started: now(),
            worker,
            microphone: Microphone::Free,
            retiring_insertions: Vec::new(),
            snapshots,
            epoch,
            outcome: Outcome::Idle,
            notice: Some(Notice::LoadingModel),
            desktop_ready,
            shortcut: speakeasy_platform::SHORTCUT.into(),
            cancel_shortcut: speakeasy_platform::CANCEL_SHORTCUT.into(),
        }
    }

    async fn wait(&mut self) -> Wake<P::Speech> {
        let timer = or_pending(
            self.gesture
                .deadline()
                .map(|at| tokio::time::sleep_until(at.into())),
        );
        let insertion = or_pending(self.session.as_mut().and_then(Session::insertion));
        // Biased: stop outranks the input lane that `request_stop` closes beside it, so a pause
        // never reads as a shortcut failure.
        tokio::select! {
            biased;
            _ = self.stopping.recv() => Wake::Stop,
            input = self.inputs.recv() => Wake::Input(input),
            changed = self.changes.changed() => Wake::Configuration(changed),
            event = self.captures.recv() => Wake::Capture(event),
            () = self.microphone.retired() => Wake::Retired,
            completion = self.worker.completed() => Wake::Worker(completion),
            result = insertion => Wake::Insertion(result),
            () = timer => Wake::Timer,
        }
    }

    fn handle(&mut self, wake: Wake<P::Speech>) -> ControlFlow<()> {
        let action = match wake {
            Wake::Stop | Wake::Configuration(Err(_)) => return ControlFlow::Break(()),
            Wake::Input(input) => self.handle_input(input)?,
            Wake::Configuration(Ok(())) => {
                self.configure();
                None
            },
            Wake::Capture(Ok(event)) => {
                self.handle_capture(event);
                None
            },
            Wake::Capture(Err(_)) | Wake::Retired => None,
            Wake::Worker(completion) => {
                self.handle_worker(completion);
                None
            },
            Wake::Insertion(result) => {
                self.handle_insertion(result);
                None
            },
            Wake::Timer => self.gesture.tick(now()),
        };
        self.handle_action(action)
    }

    fn handle_input(&mut self, input: Result<Input, RecvError>) -> ControlFlow<(), Option<Action>> {
        let Ok(input) = input else {
            return self.stop_with_failure(
                "Shortcut monitoring stopped. Open Speakeasy and enable dictation again.".into(),
            );
        };
        let action = match input {
            Input::Press if self.desktop_ready => self.gesture.press(now()),
            Input::Toggle if self.desktop_ready => self.gesture.toggle(now()),
            Input::Press | Input::Toggle => None,
            Input::Release => self.gesture.release(now()),
            Input::Lock => self.gesture.lock(),
            Input::Cancel => self.gesture.cancel(),
            Input::Unavailable(message) => return self.stop_with_failure(message),
            Input::DesktopReady { shortcut, cancel } => {
                self.desktop_ready = true;
                self.shortcut = shortcut.into();
                self.cancel_shortcut = cancel.into();
                None
            },
        };
        ControlFlow::Continue(action)
    }

    fn stop_with_failure<T>(&mut self, message: String) -> ControlFlow<(), T> {
        self.fail(message);
        self.publish();
        ControlFlow::Break(())
    }

    fn configure(&mut self) {
        let next = self.changes.borrow_and_update().clone();
        let reload = self.config.speech_changed(&next);
        self.config = next;
        self.gesture.cancel();
        if reload {
            self.worker.reload(&self.ports, &self.config);
        }
        self.abandon();
        self.worker.revive(&self.ports, &self.config);
        self.outcome = Outcome::Idle;
        self.notice = Some(if self.worker.is_ready() {
            Notice::SettingsApplied
        } else {
            Notice::LoadingModel
        });
    }

    fn handle_capture(&mut self, event: CaptureEvent) {
        let gesture = self.gesture.state();
        let Some(session) = self.current_capture_mut(event.session()) else {
            return;
        };
        match event {
            CaptureEvent::Ready(_, device) => {
                session.timeline.audio(now(), device);
                session.audio_ready(gesture);
            },
            CaptureEvent::Level(_, level) => session.show_level(level),
            CaptureEvent::Paused(_, sequence, wav) => session.paused(sequence, wav),
            CaptureEvent::Finished(_, Ok(Some(Captured { wav, speculated }))) => {
                session
                    .timeline
                    .sealed(now(), audio::wav_duration(&wav).unwrap_or_default());
                let recognized = session.seal(wav, speculated);
                self.conclude_capture();
                if let Some(text) = recognized {
                    self.deliver_speculation(text);
                }
            },
            CaptureEvent::Finished(_, Ok(None)) => {
                self.conclude_capture();
                self.end_session();
                self.gesture.complete();
                self.outcome = Outcome::Empty;
            },
            CaptureEvent::Finished(_, Err(error)) => {
                self.conclude_capture();
                self.end_session();
                self.fail(error.to_string());
            },
        }
    }

    /// The only way a capture event reaches a session: the identity must match and the session must
    /// still be capturing, so late audio from a completed capture is ignored.
    fn current_capture_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.session
            .as_mut()
            .filter(|session| session.id == id && session.is_capturing())
    }

    fn conclude_capture(&mut self) {
        self.microphone.retire();
        self.gesture.finish();
    }

    fn handle_worker(&mut self, completion: Completion<P::Speech>) {
        match completion {
            Completion::Loaded(Ok(worker)) => {
                self.worker = Worker::Ready(worker);
                if matches!(self.notice, Some(Notice::LoadingModel)) {
                    self.notice = None;
                }
            },
            Completion::Loaded(Err(error)) if self.session_survives_warmup_failure() => {
                self.notice = Some(Notice::WarmupFailed(error.to_string()));
            },
            Completion::Loaded(Err(error)) => self.fail(error.to_string()),
            Completion::Transcribed(result) => {
                let speculation = self
                    .session
                    .as_mut()
                    .and_then(|session| session.speculating.take());
                match (speculation, result) {
                    (Some(sequence), Ok((worker, Transcript::Text(text)))) => {
                        self.worker = Worker::Ready(worker);
                        self.speculation_recognized(sequence, text);
                    },
                    (Some(_), Ok((worker, Transcript::Cancelled))) => {
                        self.worker = Worker::Ready(worker);
                        self.speculation_lost();
                    },
                    // A failed speculation leaves the recording's own request to a fresh worker.
                    (Some(_), Err(_)) => {
                        self.speculation_lost();
                        self.worker.revive(&self.ports, &self.config);
                    },
                    (None, Ok((worker, transcript))) => {
                        self.worker = Worker::Ready(worker);
                        if let Transcript::Text(text) = transcript {
                            self.insert_transcript(text);
                        }
                    },
                    (None, Err(error)) => {
                        self.fail(error.to_string());
                        self.worker.revive(&self.ports, &self.config);
                    },
                }
            },
        }
    }

    /// Keeps a pause's text for the end of the recording, or delivers it when the sealed recording
    /// waits on this pause because its audio is identical. Otherwise the text is obsolete.
    fn speculation_recognized(&mut self, sequence: u32, text: String) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.is_capturing() {
            session.speculated = Some((sequence, text));
        } else if let Stage::AwaitingSpeculation(wav) = &mut session.stage {
            wav.fill(0);
            self.deliver_speculation(text);
        }
    }

    /// A recording that waited on a failed or cancelled speculation makes its own request.
    fn speculation_lost(&mut self) {
        if let Some(session) = &mut self.session
            && let Stage::AwaitingSpeculation(wav) = &mut session.stage
        {
            session.stage = Stage::AwaitingWorker(mem::take(wav));
        }
    }

    /// A session not yet waiting for the worker survives a failed warmup: its gesture finishes on
    /// its own terms, and an owned microphone must be released before a retry may start.
    fn session_survives_warmup_failure(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| !matches!(session.stage, Stage::AwaitingWorker(_)))
    }

    /// Only the current job's transcript arrives here: abandoning a session first moves its job out
    /// of observation, so stale text can never be inserted.
    fn insert_transcript(&mut self, text: String) {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| matches!(session.stage, Stage::Transcribing))
        else {
            return;
        };
        session.timeline.transcribed(now());
        self.deliver(text);
    }

    /// Delivers text recognized from a pause whose audio matches the sealed recording exactly.
    fn deliver_speculation(&mut self, text: String) {
        if let Some(session) = &mut self.session {
            session.timeline.speculated(now());
        }
        self.deliver(text);
    }

    /// Inserts the current session's text, or ends it as empty.
    fn deliver(&mut self, text: String) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if text.is_empty() {
            self.abandon();
            self.gesture.complete();
            self.outcome = Outcome::Empty;
        } else {
            let insertion = self
                .ports
                .insert(text, session.permit.clone(), self.config.delivery());
            session.stage = Stage::Inserting(tokio::spawn(insertion));
        }
    }

    fn handle_insertion(&mut self, result: Result<anyhow::Result<Inserted>, JoinError>) {
        // `wait` already joined this insertion, so the session holds no work left to retire.
        self.end_session();
        self.outcome = match result {
            Ok(Ok(Inserted::Sent)) => Outcome::Done,
            Ok(Ok(Inserted::Cancelled)) => Outcome::Cancelled,
            Ok(Ok(Inserted::Unavailable(message) | Inserted::Copied(message))) => {
                Outcome::Failed(message.into())
            },
            Ok(Err(_)) | Err(_) => Outcome::Failed(
                "Could not submit text. Check desktop permissions and clipboard access, then try again."
                    .into(),
            ),
        };
        self.gesture.complete();
    }

    fn handle_action(&mut self, action: Option<Action>) -> ControlFlow<()> {
        match action {
            Some(Action::Start) => return self.start_session(),
            Some(Action::Finish) => {
                self.microphone.finish();
                if let Some(session) = &mut self.session {
                    session.timeline.released(now());
                    session.finish();
                }
            },
            Some(Action::Cancel) => {
                self.abandon();
                self.worker.revive(&self.ports, &self.config);
                self.outcome = Outcome::Cancelled;
            },
            Some(Action::ModeChanged) => self.speculate_when_hands_free(),
            None => {},
        }
        ControlFlow::Continue(())
    }

    /// Hands-free capture ends with a separate press, usually well after the last word, so pauses
    /// are worth recognizing early. A held shortcut is released too soon after speech to benefit.
    fn speculate_when_hands_free(&self) {
        if self.gesture.state() == State::HandsFree {
            self.microphone.speculate();
        }
    }

    fn start_session(&mut self) -> ControlFlow<()> {
        let Some(permit) = self.permits.begin() else {
            return ControlFlow::Break(());
        };
        let Some(id) = self.latest_id.next() else {
            return ControlFlow::Break(());
        };
        self.latest_id = id;
        self.latest_started = now();
        self.outcome = Outcome::Idle;
        self.notice = None;
        self.session = Some(Session::new(id, permit, self.latest_started));
        ControlFlow::Continue(())
    }

    fn end_session(&mut self) {
        if let Some(session) = self.take_session() {
            session.permit.revoke();
        }
    }

    /// Removes the session, keeping its timeline until the owner reports how it ended.
    fn take_session(&mut self) -> Option<Session> {
        let session = self.session.take()?;
        self.ended = Some((session.timeline, now()));
        Some(session)
    }

    /// Hands the ended session's timings to the ports once this wake has settled its outcome.
    fn report_ended(&mut self) {
        if let Some((timeline, ended)) = self.ended.take() {
            self.ports
                .report_timing(timeline.report(ended, self.outcome.label()));
        }
    }

    /// Revokes the session and hands its native work to cleanup without awaiting any OS call.
    fn abandon(&mut self) {
        let Some(mut session) = self.take_session() else {
            return;
        };
        session.permit.revoke();
        if let Some((_, mut pending)) = session.pending.take() {
            pending.fill(0);
        }
        match session.stage {
            Stage::Opening | Stage::Recording | Stage::Stopping => self.microphone.retire(),
            Stage::Inserting(task) => self.retiring_insertions.push(task),
            Stage::AwaitingWorker(mut wav) | Stage::AwaitingSpeculation(mut wav) => wav.fill(0),
            Stage::Transcribing if self.worker.is_transcribing() => {
                self.worker.recover(&self.ports, &self.config);
            },
            Stage::Queued | Stage::Transcribing => {},
        }
    }

    fn fail(&mut self, message: String) {
        self.gesture.cancel();
        self.abandon();
        self.outcome = Outcome::Failed(message);
    }

    async fn advance(&mut self) {
        self.open_queued_capture();
        self.start_transcription();
        self.reap_finished_insertions().await;
    }

    fn open_queued_capture(&mut self) {
        if self.permits.is_closed() || !self.microphone.is_free() {
            return;
        }
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| matches!(session.stage, Stage::Queued))
        else {
            return;
        };
        let microphone = self.config.microphone.as_deref();
        match self
            .ports
            .record(session.id, microphone, self.events.clone())
        {
            Ok(recording) => {
                if self.gesture.state() == State::Processing {
                    recording.finish();
                }
                self.microphone = Microphone::Open(recording);
                session.stage = Stage::Opening;
                self.speculate_when_hands_free();
            },
            Err(error) => self.fail(format!("Could not start microphone worker: {error}")),
        }
    }

    fn start_transcription(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.is_capturing() {
            // Speculation only uses an idle warm worker; it never loads one.
            if self.worker.is_ready()
                && let Some((sequence, wav)) = session.pending.take()
            {
                match self.worker.transcribe(wav, &self.config) {
                    Ok(()) => session.speculating = Some(sequence),
                    Err(unclaimed) => session.pending = Some((sequence, unclaimed)),
                }
            }
            return;
        }
        let Stage::AwaitingWorker(wav) = &mut session.stage else {
            return;
        };
        if self.worker.revive(&self.ports, &self.config) {
            self.notice = Some(Notice::LoadingModel);
        }
        match self.worker.transcribe(mem::take(wav), &self.config) {
            Ok(()) => {
                session.timeline.transcribing(now());
                session.stage = Stage::Transcribing;
            },
            Err(unclaimed) => *wav = unclaimed,
        }
    }

    /// Obsolete insertions finish their own cleanup; they are never aborted, and their results
    /// cannot reach the current session.
    async fn reap_finished_insertions(&mut self) {
        for task in self
            .retiring_insertions
            .extract_if(.., |task| task.is_finished())
        {
            reap(task).await;
        }
    }

    fn message(&self) -> &str {
        match &self.outcome {
            Outcome::Failed(message) => message,
            Outcome::Idle | Outcome::Done | Outcome::Empty | Outcome::Cancelled => {
                self.notice.as_ref().map_or("", Notice::text)
            },
        }
    }

    fn publish(&self) {
        debug_assert_eq!(self.gesture.state() == State::Idle, self.session.is_none());
        let session = self.session.as_ref();
        let snapshot = Snapshot {
            epoch: self.epoch,
            id: self.latest_id.into(),
            phase: session.map_or_else(|| self.outcome.phase(), Session::phase),
            hands_free: self.gesture.state() == State::HandsFree,
            level: session.map_or(0.0, |session| session.level),
            meter_tick: session.map_or(0, |session| session.meter_tick),
            started: self.latest_started,
            message: self.message().to_owned(),
            model: self.worker.model(),
            desktop_ready: self.desktop_ready,
            shortcut: self.shortcut.clone(),
            cancel_shortcut: self.cancel_shortcut.clone(),
        };
        self.snapshots.send_if_modified(|published| {
            if published.epoch != self.epoch || *published == snapshot {
                return false;
            }
            *published = snapshot;
            true
        });
    }

    async fn shutdown(mut self) {
        self.permits.close();
        self.captures.close();
        self.abandon();
        let insertions = async {
            for task in self.retiring_insertions {
                reap(task).await;
            }
        };
        tokio::join!(self.worker.stop(), self.microphone.stop(), insertions);
    }
}

pub(super) async fn run<P: Ports>(wiring: Wiring<P>) {
    let mut owner = Owner::new(wiring);
    owner.publish();
    loop {
        let wake = owner.wait().await;
        if owner.handle(wake).is_break() {
            break;
        }
        owner.advance().await;
        owner.report_ended();
        owner.publish();
    }
    owner.shutdown().await;
}

/// Pends unless a future is present; resolves once it completes.
async fn or_pending<F: Future>(future: Option<F>) -> F::Output {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}
