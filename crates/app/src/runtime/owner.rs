//! The single session authority. Each wake handles one message, advances work whose prerequisites
//! are satisfied, then publishes a projection of owned state.

use std::{convert::Infallible, mem, ops::ControlFlow, sync::Arc, time::Instant};

use async_channel::{Receiver, RecvError, Sender};
use speakeasy_core::gesture::{Action, Gesture, State};
use speakeasy_platform::{Input, InputSender, Inserted};
use speakeasy_show::Narration;
use tokio::{sync::watch, task::JoinError};

use super::{
    CaptureEvent, LOADING, Phase, ShowEvent, Snapshot, Wiring,
    device::Device,
    now, reap,
    session::{Delivered, InsertTask, PublishTask, Session, SessionId, Show, Stage},
    worker::{Completion, Detail, Transcript, Worker},
};
use crate::{
    config::Config,
    ports::{Ports, Published, Recording},
};

/// What the status shows while no session is active: the latest result or failure.
enum Outcome {
    Idle,
    Done,
    /// A Show's bundle was written and its summary copied.
    Shown,
    Empty,
    Cancelled,
    Failed(String),
}

impl Outcome {
    const fn phase(&self) -> Phase {
        match self {
            Self::Idle => Phase::Idle,
            Self::Done | Self::Shown => Phase::Done,
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
    Show(Result<ShowEvent, RecvError>),
    Worker(Completion<W>),
    Insertion(Result<anyhow::Result<Inserted>, JoinError>),
    Publication(Result<anyhow::Result<Published>, JoinError>),
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
    show_events: Sender<ShowEvent>,
    shows: Receiver<ShowEvent>,

    permits: InputSender,
    gesture: Gesture,
    session: Option<Session>,
    latest_id: SessionId,
    latest_started: Instant,

    worker: Worker<P::Speech>,
    microphone: Device<P::Recording>,
    screen: Device<P::Show>,
    retiring_insertions: Vec<InsertTask>,
    retiring_publications: Vec<PublishTask>,
    /// Whether the latest session was a Show, which the pill presents differently.
    latest_show: bool,

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
        let (show_events, shows) = async_channel::bounded(4);
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
            show_events,
            shows,
            permits,
            gesture: Gesture::default(),
            session: None,
            latest_id: SessionId::default(),
            latest_started: now(),
            worker,
            microphone: Device::Free,
            screen: Device::Free,
            retiring_insertions: Vec::new(),
            retiring_publications: Vec::new(),
            latest_show: false,
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
        let delivered = or_pending(self.session.as_mut().map(Session::delivered));
        // Biased: stop outranks the input lane that `request_stop` closes beside it, so a pause
        // never reads as a shortcut failure.
        tokio::select! {
            biased;
            _ = self.stopping.recv() => Wake::Stop,
            input = self.inputs.recv() => Wake::Input(input),
            changed = self.changes.changed() => Wake::Configuration(changed),
            event = self.captures.recv() => Wake::Capture(event),
            event = self.shows.recv() => Wake::Show(event),
            () = self.microphone.retired() => Wake::Retired,
            () = self.screen.retired() => Wake::Retired,
            completion = self.worker.completed() => Wake::Worker(completion),
            delivered = delivered => match delivered {
                Delivered::Insertion(result) => Wake::Insertion(result),
                Delivered::Publication(result) => Wake::Publication(result),
            },
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
            Wake::Show(Ok(event)) => {
                self.handle_show(event);
                None
            },
            Wake::Capture(Err(_)) | Wake::Show(Err(_)) | Wake::Retired => None,
            Wake::Worker(completion) => {
                self.handle_worker(completion);
                None
            },
            Wake::Insertion(result) => {
                self.handle_insertion(result);
                None
            },
            Wake::Publication(result) => {
                self.handle_publication(result);
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
            Input::Show => self.start_show(),
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
            CaptureEvent::Ready(_) => {
                session.audio_started.get_or_insert_with(now);
                session.audio_ready(gesture);
            },
            CaptureEvent::Level(_, level) => session.show_level(level),
            CaptureEvent::Finished(_, Ok(Some(sealed))) => {
                if let Some(show) = &mut session.show {
                    show.trimmed = sealed.trimmed;
                }
                session.stage = Stage::AwaitingWorker(sealed.wav);
                self.conclude_capture();
            },
            // A Show is worth sending without a word said: its ink and clicks still tell.
            CaptureEvent::Finished(_, Ok(None)) if session.show.is_some() => {
                session.narrate(Narration::default());
                session.stage = Stage::AwaitingFootage;
                self.conclude_capture();
            },
            CaptureEvent::Finished(_, Ok(None)) => {
                self.conclude_capture();
                self.end_session();
                self.gesture.complete();
                self.outcome = Outcome::Empty;
            },
            CaptureEvent::Finished(_, Err(error)) => {
                self.conclude_capture();
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

    /// Ends capture however it ended, finishing a Show's screen with its narration.
    fn conclude_capture(&mut self) {
        self.microphone.retire();
        self.screen.finish();
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
            Completion::Transcribed(Ok((worker, transcript))) => {
                self.worker = Worker::Ready(worker);
                match transcript {
                    Transcript::Text(text) => self.insert_transcript(text),
                    Transcript::Words(narration) => self.narrate(narration),
                    Transcript::Cancelled => {},
                }
            },
            Completion::Transcribed(Err(error)) => {
                self.fail(error.to_string());
                self.worker.revive(&self.ports, &self.config);
            },
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

    /// Only the current Show's narration arrives here, for the same reason as a transcript.
    fn narrate(&mut self, narration: Narration) {
        if let Some(session) = self
            .session
            .as_mut()
            .filter(|session| matches!(session.stage, Stage::Transcribing))
        {
            session.narrate(narration);
            session.stage = Stage::AwaitingFootage;
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

    /// Turns the held recording into a Show, keeping the narration already captured, and locks it
    /// hands-free; the next press finishes it.
    fn start_show(&mut self) -> Option<Action> {
        let held = self.gesture.state() == State::Held;
        let session = self
            .session
            .as_mut()
            .filter(|session| held && session.is_capturing() && session.show.is_none())?;
        session.show = Some(Show::default());
        self.latest_show = true;
        self.gesture.lock()
    }

    fn handle_show(&mut self, event: ShowEvent) {
        let ShowEvent::Finished(id, footage) = event;
        let Some(show) = self
            .session
            .as_mut()
            .filter(|session| session.id == id)
            .and_then(|session| session.show.as_mut())
        else {
            return;
        };
        match footage {
            Ok(footage) => {
                show.footage = Some(footage);
                self.screen.retire();
            },
            Err(error) => self.fail(format!("{error:#}")),
        }
    }

    fn handle_publication(&mut self, result: Result<anyhow::Result<Published>, JoinError>) {
        // `wait` already joined this publication, so the session holds no work left to retire.
        self.end_session();
        self.outcome = match result {
            Ok(Ok(Published::Copied(_))) => Outcome::Shown,
            Ok(Ok(Published::Cancelled)) => Outcome::Cancelled,
            Ok(Err(error)) => Outcome::Failed(format!("{error:#}")),
            Err(_) => {
                Outcome::Failed("Publishing the Show stopped unexpectedly. Try again.".into())
            },
        };
        self.gesture.complete();
    }

    fn handle_action(&mut self, action: Option<Action>) -> ControlFlow<()> {
        match action {
            Some(Action::Start) => return self.start_session(),
            Some(Action::Finish) => {
                self.microphone.finish();
                self.screen.finish();
                if let Some(session) = &mut self.session {
                    session.finish();
                }
            },
            Some(Action::Cancel) => {
                self.abandon();
                self.worker.revive(&self.ports, &self.config);
                self.outcome = Outcome::Cancelled;
            },
            Some(Action::ModeChanged) | None => {},
        }
        ControlFlow::Continue(())
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
        self.latest_show = false;
        self.outcome = Outcome::Idle;
        self.notice = None;
        self.session = Some(Session::new(id, permit, self.latest_started));
        ControlFlow::Continue(())
    }

    fn end_session(&mut self) {
        if let Some(session) = self.session.take() {
            session.permit.revoke();
        }
    }

    /// Revokes the session and hands its native work to cleanup without awaiting any OS call.
    fn abandon(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        session.permit.revoke();
        if session.show.is_some() {
            self.screen.retire();
        }
        match session.stage {
            Stage::Opening | Stage::Recording | Stage::Stopping => self.microphone.retire(),
            Stage::Inserting(task) => self.retiring_insertions.push(task),
            Stage::Publishing(task) => self.retiring_publications.push(task),
            Stage::AwaitingWorker(mut wav) => wav.fill(0),
            Stage::Transcribing if self.worker.is_transcribing() => {
                self.worker.recover(&self.ports, &self.config);
            },
            Stage::Queued | Stage::Transcribing | Stage::AwaitingFootage => {},
        }
    }

    fn fail(&mut self, message: String) {
        self.gesture.cancel();
        self.abandon();
        self.outcome = Outcome::Failed(message);
    }

    async fn advance(&mut self) {
        self.open_queued_capture();
        self.open_queued_screen();
        self.start_transcription();
        self.start_publishing();
        self.reap_retired_work().await;
    }

    /// Opens a Show's screen recorder once any previous one has retired, even after its narration
    /// ended, so the Show still gets the screen as it was then.
    fn open_queued_screen(&mut self) {
        if self.permits.is_closed() || !self.screen.is_free() {
            return;
        }
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.show.as_ref().is_some_and(|show| !show.opened))
        else {
            return;
        };
        match self
            .ports
            .show(session.id, session.started, self.show_events.clone())
        {
            Ok(recorder) => {
                if !session.is_capturing() || self.gesture.state() == State::Processing {
                    recorder.finish();
                }
                self.screen = Device::Open(recorder);
                if let Some(show) = &mut session.show {
                    show.opened = true;
                }
            },
            Err(error) => self.fail(format!("Could not record the screen: {error:#}")),
        }
    }

    /// Publishes a Show once its narration and screen recording are both in.
    fn start_publishing(&mut self) {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| matches!(session.stage, Stage::AwaitingFootage))
        else {
            return;
        };
        if let Some(recorded) = session.take_recorded() {
            let publication = self.ports.publish(recorded, session.permit.clone());
            session.stage = Stage::Publishing(tokio::spawn(publication));
        }
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
                self.microphone = Device::Open(recording);
                session.stage = Stage::Opening;
            },
            Err(error) => self.fail(format!("Could not start microphone worker: {error}")),
        }
    }

    fn start_transcription(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let detail = if session.show.is_some() {
            Detail::Words
        } else {
            Detail::Text
        };
        let Stage::AwaitingWorker(wav) = &mut session.stage else {
            return;
        };
        if self.worker.revive(&self.ports, &self.config) {
            self.notice = Some(Notice::LoadingModel);
        }
        match self.worker.transcribe(mem::take(wav), detail, &self.config) {
            Ok(()) => session.stage = Stage::Transcribing,
            Err(unclaimed) => *wav = unclaimed,
        }
    }

    /// Obsolete insertions and publications finish their own cleanup; they are never aborted, and
    /// their results cannot reach the current session.
    async fn reap_retired_work(&mut self) {
        for task in self
            .retiring_insertions
            .extract_if(.., |task| task.is_finished())
        {
            reap(task).await;
        }
        for task in self
            .retiring_publications
            .extract_if(.., |task| task.is_finished())
        {
            reap(task).await;
        }
    }

    fn message(&self) -> &str {
        match &self.outcome {
            Outcome::Failed(message) => message,
            Outcome::Shown => "Show copied. Paste it into your coding agent.",
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
            show: self.latest_show,
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
        let retired = async {
            for task in self.retiring_insertions {
                reap(task).await;
            }
            for task in self.retiring_publications {
                reap(task).await;
            }
        };
        tokio::join!(
            self.worker.stop(),
            self.microphone.stop(),
            self.screen.stop(),
            retired
        );
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
