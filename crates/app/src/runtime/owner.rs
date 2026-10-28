//! The single session authority. Each wake handles one message, advances work whose prerequisites
//! are satisfied, then publishes a projection of owned state.

use std::{convert::Infallible, ops::ControlFlow, sync::Arc, time::Instant};

use async_channel::{Receiver, RecvError, Sender};
use speakeasy_core::gesture::{Action, Gesture, State};
use speakeasy_platform::{Input, InputSender, Inserted, Scope};
use speakeasy_show::Narration;
use tokio::{
    sync::watch,
    task::{JoinError, JoinHandle},
};

use super::{
    LOADING, Meter, Phase, Snapshot, Wiring,
    device::Device,
    now,
    ports::{CaptureEvent, Capturing, PUBLISH_STOPPED, Ports, Published, SessionId, ShowEvent},
    reap,
    session::{Delivered, Session},
    worker::{Completion, Detail, Transcript, Worker},
};
use crate::{
    config::{Change, Config},
    sounds::Cue,
};

/// What a copied Show tells the user.
const SHOW_COPIED: &str = "Show copied. Paste it into your coding agent.";

/// What the status shows while no session is active: the latest result or failure.
enum Outcome {
    Idle,
    Done,
    /// The delivery is on the clipboard; the message says how to paste it.
    Copied(&'static str),
    Empty,
    Cancelled,
    /// The pill and Settings show this message as is, so it never includes an error source that
    /// could carry private text, such as a panic payload or engine output.
    Failed(String),
}

impl Outcome {
    const fn phase(&self) -> Phase {
        match self {
            Self::Idle => Phase::Idle,
            Self::Done => Phase::Done,
            Self::Copied(_) => Phase::Copied,
            Self::Empty => Phase::Empty,
            Self::Cancelled => Phase::Cancelled,
            Self::Failed(_) => Phase::Error,
        }
    }
}

/// Status text shown unless the outcome carries its own message: a failure or a copied delivery.
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
    Report(Result<String, RecvError>),
    Configuration(Result<(), watch::error::RecvError>),
    Capture(Result<CaptureEvent, RecvError>),
    Show(Result<ShowEvent, RecvError>),
    Worker(Completion<W>),
    Delivered(Result<Delivered, JoinError>),
    Retired,
    Timer,
}

struct Owner<P: Ports> {
    ports: P,
    config: Config,

    changes: watch::Receiver<Config>,
    inputs: Receiver<Input>,
    reports: Receiver<String>,
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
    microphone: Device<P::Microphone>,
    screen: Device<P::Screen>,
    /// Abandoned insertions and publications, kept until they finish their own cleanup.
    retiring: Vec<JoinHandle<Delivered>>,
    /// Whether the latest session was a Show, which the pill presents differently.
    latest_show: bool,
    /// Whether the cue output is held open, as it is through every session while sounds are on.
    cues_held: bool,

    snapshots: watch::Sender<Snapshot>,
    meter: watch::Sender<Meter>,
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
            reports,
            snapshots,
            meter,
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
            reports,
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
            retiring: Vec::new(),
            latest_show: false,
            cues_held: false,
            snapshots,
            meter,
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
            report = self.reports.recv() => Wake::Report(report),
            changed = self.changes.changed() => Wake::Configuration(changed),
            event = self.captures.recv() => Wake::Capture(event),
            event = self.shows.recv() => Wake::Show(event),
            () = self.microphone.retired() => Wake::Retired,
            () = self.screen.retired() => Wake::Retired,
            completion = self.worker.completed() => Wake::Worker(completion),
            delivered = delivered => Wake::Delivered(delivered),
            () = timer => Wake::Timer,
        }
    }

    fn handle(&mut self, wake: Wake<P::Speech>) -> ControlFlow<()> {
        let action = match wake {
            // Only `Runtime` holds the report and configuration senders, and dropping it stops first.
            Wake::Stop | Wake::Report(Err(_)) | Wake::Configuration(Err(_)) => {
                return ControlFlow::Break(());
            },
            Wake::Input(input) => self.handle_input(input)?,
            Wake::Report(Ok(message)) => {
                // The owner's own state, not a published copy, decides idleness, so a session that
                // started meanwhile is never overwritten.
                if self.session.is_none() {
                    self.outcome = Outcome::Failed(message);
                }
                None
            },
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
            Wake::Delivered(result) => {
                self.handle_delivered(result);
                None
            },
            Wake::Timer => self.gesture.tick(now()),
        };
        self.handle_action(action)
    }

    fn handle_input(&mut self, input: Result<Input, RecvError>) -> ControlFlow<(), Option<Action>> {
        let Ok(input) = input else {
            return self.stop_with_failure(
                "Shortcut monitoring stopped. Resume dictation to try again.".into(),
            );
        };
        let action = match input {
            Input::Press if self.desktop_ready => self.gesture.press(now()),
            Input::Toggle if self.desktop_ready => self.gesture.toggle(now()),
            Input::Press | Input::Toggle => None,
            Input::Release => self.gesture.release(now()),
            Input::Lock => self.gesture.lock(),
            Input::Show(scope) => self.start_show(scope),
            Input::Pin(taps) => {
                self.screen.pin(taps);
                None
            },
            Input::Cancel => self.gesture.cancel(),
            // Another shortcut or a session change took the keys: the recording ends without the
            // cue that confirms a cancel the user never asked for.
            Input::Interrupt => {
                if self.gesture.cancel().is_some() {
                    self.discard();
                }
                None
            },
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
        let reload = self.config.change_to(&next) >= Change::Engine;
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
                session.audio_ready(gesture, now());
                // The pill opens to listening here, so the cue sounds with it, never before the
                // microphone listens.
                let cue = if session.is_show() {
                    Cue::Show
                } else {
                    Cue::Start
                };
                if session.is_recording() {
                    self.cue_heard(cue);
                }
            },
            CaptureEvent::Level(id, level) => {
                self.meter.send_replace(Meter {
                    epoch: self.epoch,
                    session: id.into(),
                    level,
                });
            },
            CaptureEvent::Finished(_, Ok(sealed)) => {
                let delivers = session.captured(sealed);
                self.conclude_capture();
                if !delivers {
                    self.end_session(Outcome::Empty);
                }
            },
            // Failing retires both devices; the screen is never asked to finish footage for nobody.
            CaptureEvent::Finished(_, Err(error)) => self.fail(error.to_string()),
        }
    }

    /// The only way a capture event reaches a session: the identity must match and the session must
    /// still be capturing, so late audio from a completed capture is ignored.
    fn current_capture_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.session
            .as_mut()
            .filter(|session| session.id == id && session.is_capturing())
    }

    /// Ends capture however it ended, finishing a Show's screen with its narration. A capture that
    /// ended before the gesture did sounds the stop cue the gesture would have.
    fn conclude_capture(&mut self) {
        self.microphone.retire();
        self.screen.finish();
        if self.gesture.finish().is_some() {
            self.cue(Cue::Stop);
        }
    }

    fn handle_worker(&mut self, completion: Completion<P::Speech>) {
        match completion {
            Completion::Loaded(Ok(worker)) => {
                self.worker = Worker::Ready(worker);
                if matches!(
                    self.notice,
                    Some(Notice::LoadingModel | Notice::WarmupFailed(_))
                ) {
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
                }
            },
            Completion::Transcribed(Err(error)) => {
                self.fail(error.to_string());
                self.worker.revive(&self.ports, &self.config);
            },
        }
    }

    /// A session that is not waiting for the worker survives a failed warmup: its gesture finishes
    /// on its own terms, and an owned microphone must be released before a retry may start.
    fn session_survives_warmup_failure(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| !session.awaits_worker())
    }

    /// Only the current job's transcript arrives here: abandoning a session first moves its job out
    /// of observation, so stale text can never be inserted.
    fn insert_transcript(&mut self, text: String) {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.is_transcribing())
        else {
            return;
        };
        if text.is_empty() {
            self.end_session(Outcome::Empty);
        } else {
            let (ports, delivery) = (&self.ports, self.config.delivery());
            session.insert(|permit| ports.insert(text, permit, delivery));
        }
    }

    /// Only the current Show's narration arrives here, for the same reason as a transcript.
    fn narrate(&mut self, narration: Narration) {
        if let Some(session) = &mut self.session {
            session.narrate(narration);
        }
    }

    /// Turns the held recording into a Show of what `scope` names, keeping the narration already
    /// captured, and locks it hands-free; the next press finishes it. The keyboard policy sends a
    /// Show only for a press during a physical hold, and its repeats, so a recording already locked
    /// hands-free with Space or a double tap turns too, as does one still waiting for the
    /// microphone.
    fn start_show(&mut self, scope: Scope) -> Option<Action> {
        let held = matches!(self.gesture.state(), State::Held | State::HandsFree);
        let session = self.session.as_mut().filter(|_| held)?;
        if !session.turn_into_show(scope) {
            return None;
        }
        self.latest_show = true;
        // A microphone still opening sounds the Show cue once it listens.
        if session.is_recording() {
            self.cue_heard(Cue::Show);
        }
        self.gesture.lock()
    }

    fn handle_show(&mut self, event: ShowEvent) {
        let ShowEvent::Finished(id, footage) = event;
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.id == id && session.is_show())
        else {
            return;
        };
        match footage {
            Ok(footage) => {
                session.footage(footage);
                self.screen.retire();
            },
            Err(error) => self.fail(format!("{error:#}")),
        }
    }

    fn handle_delivered(&mut self, result: Result<Delivered, JoinError>) {
        // Only a Show publishes, and a session cannot become one once transcription starts.
        let show = self.session.as_ref().is_some_and(Session::is_show);
        let outcome = match result {
            Ok(Delivered::Inserted(Ok(Inserted::Sent))) => Outcome::Done,
            Ok(Delivered::Inserted(Ok(Inserted::Copied(message)))) => {
                self.cue(Cue::Copied);
                Outcome::Copied(message)
            },
            Ok(
                Delivered::Inserted(Ok(Inserted::Cancelled))
                | Delivered::Published(Ok(Published::Cancelled)),
            ) => Outcome::Cancelled,
            Ok(Delivered::Inserted(Ok(Inserted::Unavailable(message)))) => {
                Outcome::Failed(message.into())
            },
            Ok(Delivered::Published(Ok(Published::Copied))) => {
                self.cue(Cue::Copied);
                Outcome::Copied(SHOW_COPIED)
            },
            Ok(Delivered::Published(Err(error))) => Outcome::Failed(format!("{error:#}")),
            Err(_) if show => Outcome::Failed(PUBLISH_STOPPED.into()),
            Ok(Delivered::Inserted(Err(_))) | Err(_) => Outcome::Failed(
                "Could not submit text. Check desktop permissions and clipboard access, then try again."
                    .into(),
            ),
        };
        // `wait` already joined this delivery, so the session holds no work left to retire.
        self.end_session(outcome);
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
                // The microphone has stopped taking samples, so it cannot hear this.
                self.cue(Cue::Stop);
            },
            Some(Action::Cancel) => {
                self.discard();
                self.cue(Cue::Cancel);
            },
            Some(Action::ModeChanged) | None => {},
        }
        ControlFlow::Continue(())
    }

    fn start_session(&mut self) -> ControlFlow<()> {
        // A closed lane still holds whatever closed it: the next wake reads a queued
        // `Unavailable`, the stop lane, or the drained lane's error through `handle_input`.
        let Some(permit) = self.permits.begin() else {
            self.gesture.cancel();
            return ControlFlow::Continue(());
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

    /// Holds the cue output open from the moment a session begins, so its first cue never waits
    /// for a device to open, and releases it as Speakeasy goes idle or sounds turn off, however the
    /// session ended.
    fn hold_cues(&mut self) {
        let held = self.config.play_sounds && self.session.is_some();
        if held != self.cues_held {
            self.cues_held = held;
            self.ports.hold_cues(held);
        }
    }

    /// Sounds `cue` while the user wants sounds.
    fn cue(&self, cue: Cue) {
        if self.config.play_sounds {
            self.ports.cue(cue);
        }
    }

    /// Sounds `cue` where the listening microphone can hear it, telling the microphone first so
    /// the speech gate listens beneath it.
    fn cue_heard(&self, cue: Cue) {
        if self.config.play_sounds {
            self.microphone.expect_cue();
            self.ports.cue(cue);
        }
    }

    /// Ends a session whose work has finished, so it holds nothing left to retire, and shows
    /// `outcome`.
    fn end_session(&mut self, outcome: Outcome) {
        if let Some(session) = self.session.take() {
            session.permit.revoke();
        }
        self.gesture.complete();
        self.outcome = outcome;
    }

    /// Discards the session, its insertion and any recognition under way, and shows it cancelled.
    fn discard(&mut self) {
        self.abandon();
        self.worker.revive(&self.ports, &self.config);
        self.outcome = Outcome::Cancelled;
    }

    /// Revokes the session and hands its native work to cleanup without awaiting any OS call.
    fn abandon(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        session.permit.revoke();
        if session.is_show() {
            self.screen.retire();
        }
        if session.is_capturing() {
            self.microphone.retire();
        }
        if session.is_transcribing() && self.worker.is_transcribing() {
            self.worker.recover(&self.ports, &self.config);
        }
        if let Some(task) = session.into_delivery() {
            self.retiring.push(task);
        }
    }

    fn fail(&mut self, message: String) {
        self.gesture.cancel();
        self.abandon();
        self.outcome = Outcome::Failed(message);
    }

    fn advance(&mut self) {
        self.open_queued_capture();
        self.open_queued_screen();
        self.start_transcription();
        self.start_publishing();
        self.reap_retired_work();
    }

    /// Opens a Show's screen recorder once any previous one has retired, even after its narration
    /// ended, so the Show still gets the screen as it was then.
    fn open_queued_screen(&mut self) {
        if self.permits.is_closed() || !self.screen.is_free() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(scope) = session.queued_screen() else {
            return;
        };
        match self
            .ports
            .record_screen(session.id, session.started, scope, self.show_events.clone())
        {
            Ok(recorder) => {
                if !session.awaits_audio() || self.gesture.state() == State::Processing {
                    recorder.finish();
                }
                self.screen = Device::Open(recorder);
                session.screen_opened();
            },
            Err(error) => self.fail(format!("Could not record the screen: {error}")),
        }
    }

    /// Publishes a Show once its narration and screen recording are both in.
    fn start_publishing(&mut self) {
        if let Some(session) = &mut self.session {
            let ports = &self.ports;
            session.publish(|footage, narration, permit| ports.publish(footage, narration, permit));
        }
    }

    fn open_queued_capture(&mut self) {
        if self.permits.is_closed() || !self.microphone.is_free() {
            return;
        }
        let Some(session) = self.session.as_mut().filter(|session| session.is_queued()) else {
            return;
        };
        let microphone = self.config.microphone.as_deref();
        match self
            .ports
            .record_microphone(session.id, microphone, self.events.clone())
        {
            Ok(recording) => {
                if self.gesture.state() == State::Processing {
                    recording.finish();
                }
                self.microphone = Device::Open(recording);
                session.opened();
            },
            Err(error) => self.fail(format!(
                "Could not open the microphone ({error}). Try again."
            )),
        }
    }

    fn start_transcription(&mut self) {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.awaits_worker())
        else {
            return;
        };
        let detail = if session.is_show() {
            Detail::Words
        } else {
            Detail::Text
        };
        if self.worker.revive(&self.ports, &self.config) {
            self.notice = Some(Notice::LoadingModel);
        }
        let (worker, config) = (&mut self.worker, &self.config);
        session.transcribe(|wav| worker.transcribe(wav, detail, config));
    }

    /// Obsolete insertions and publications finish their own cleanup; they are never aborted, and
    /// their results cannot reach the current session. A finished one is dropped with its result;
    /// an unfinished one stays, because dropping its handle would detach it from shutdown.
    fn reap_retired_work(&mut self) {
        self.retiring.retain(|task| !task.is_finished());
    }

    fn message(&self) -> &str {
        match &self.outcome {
            Outcome::Failed(message) => message,
            Outcome::Copied(message) => message,
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
            started: self.latest_started,
            message: self.message().to_owned(),
            model: self.worker.model(),
            desktop_ready: self.desktop_ready,
            shortcut: Arc::clone(&self.shortcut),
            cancel_shortcut: Arc::clone(&self.cancel_shortcut),
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
        // Nothing reads the capture lane from here on, and a capture thread reports `Finished`
        // with a blocking send even when cancelled, so a full lane would stall its teardown.
        self.captures.close();
        self.abandon();
        let retired = async {
            for task in self.retiring {
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
        owner.advance();
        owner.hold_cues();
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
