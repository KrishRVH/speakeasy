//! The single session authority. Each wake handles one message, advances work
//! whose prerequisites are satisfied, then publishes a projection of owned state.
use super::{
    Config, Event, LOADING, Phase, Ports, Recording, Snapshot,
    microphone::Microphone,
    session::{InsertTask, Session, SessionId, Stage},
    worker::{Completion, Worker},
};
use async_channel::Receiver;
use speakeasy_core::gesture::{Action, Gesture, State};
use speakeasy_platform::{Input, InputSender, Inserted};
use std::time::Instant;
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Feedback {
    Idle,
    Done,
    Empty,
    Cancelled,
    Error,
}

impl Feedback {
    fn phase(self) -> Phase {
        match self {
            Self::Idle => Phase::Idle,
            Self::Done => Phase::Done,
            Self::Empty => Phase::Empty,
            Self::Cancelled => Phase::Cancelled,
            Self::Error => Phase::Error,
        }
    }
}

enum Wake<W> {
    Stop,
    Input(Result<Input, async_channel::RecvError>),
    Configuration(Result<(), watch::error::RecvError>),
    Audio(Result<Event, async_channel::RecvError>),
    Worker(Completion<W>),
    Insertion(Result<anyhow::Result<Inserted>, tokio::task::JoinError>),
    Retired,
    Timer,
}

struct Owner<P: Ports> {
    ports: P,
    config: Config,
    changes: watch::Receiver<Config>,
    input: Receiver<Input>,
    stopping: Receiver<()>,
    gate: InputSender,
    events: async_channel::Sender<Event>,
    audio: Receiver<Event>,
    output: watch::Sender<Snapshot>,
    epoch: u64,
    gesture: Gesture,
    session: Option<Session>,
    worker: Worker<P::Speech>,
    // These resources have lost session authority, but cleanup still owns them.
    microphone: Microphone<P::Recording>,
    retiring_insertions: Vec<InsertTask>,
    latest_id: SessionId,
    latest_started: Instant,
    feedback: Feedback,
    message: String,
    desktop_ready: bool,
    shortcut: std::sync::Arc<str>,
    cancel_shortcut: std::sync::Arc<str>,
}

pub(super) async fn run<P: Ports>(
    mut changes: watch::Receiver<Config>,
    input: Receiver<Input>,
    output: watch::Sender<Snapshot>,
    gate: InputSender,
    stopping: Receiver<()>,
    ports: P,
    epoch: u64,
) {
    let config = changes.borrow_and_update().clone();
    let (events, audio) = async_channel::bounded(64);
    let desktop_ready = !ports.prepares_desktop();
    let default = Snapshot::default();
    let mut worker = Worker::Unavailable;
    worker.replace(&ports, &config, false);
    let mut owner = Owner {
        ports,
        config,
        changes,
        input,
        stopping,
        gate,
        events,
        audio,
        output,
        epoch,
        gesture: Gesture::default(),
        session: None,
        worker,
        microphone: Microphone::Free,
        retiring_insertions: Vec::new(),
        latest_id: SessionId::default(),
        latest_started: now(),
        feedback: Feedback::Idle,
        message: LOADING.into(),
        desktop_ready,
        shortcut: default.shortcut,
        cancel_shortcut: default.cancel_shortcut,
    };
    owner.publish();
    loop {
        let wake = owner.wait().await;
        if !owner.handle(wake) {
            break;
        }
        owner.advance().await;
        owner.publish();
    }
    owner.shutdown().await;
}

fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

impl<P: Ports> Owner<P> {
    async fn wait(&mut self) -> Wake<P::Speech> {
        let deadline = self.gesture.deadline();
        let timer = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at.into()).await,
                None => std::future::pending().await,
            }
        };
        let insertion = async {
            match self.session.as_mut() {
                Some(Session {
                    stage: Stage::Inserting(task),
                    ..
                }) => task.await,
                _ => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            _ = self.stopping.recv() => Wake::Stop,
            event = self.input.recv() => Wake::Input(event),
            changed = self.changes.changed() => Wake::Configuration(changed),
            event = self.audio.recv() => Wake::Audio(event),
            _ = self.microphone.retired() => Wake::Retired,
            result = self.worker.completed() => Wake::Worker(result),
            result = insertion => Wake::Insertion(result),
            _ = timer => Wake::Timer,
        }
    }

    fn handle(&mut self, wake: Wake<P::Speech>) -> bool {
        let action = match wake {
            Wake::Stop | Wake::Configuration(Err(_)) => {
                return false;
            }
            Wake::Input(Err(_)) => {
                self.fail(
                    "Shortcut monitoring stopped. Open Speakeasy and enable dictation again."
                        .into(),
                );
                self.publish();
                return false;
            }
            Wake::Input(Ok(Input::Unavailable(message))) => {
                self.fail(message);
                self.publish();
                return false;
            }
            Wake::Input(Ok(input)) => self.handle_input(input),
            Wake::Configuration(Ok(())) => {
                self.configure();
                None
            }
            Wake::Audio(Ok(event)) => {
                self.handle_audio(event);
                None
            }
            Wake::Audio(Err(_)) => None,
            Wake::Worker(result) => {
                self.handle_worker(result);
                None
            }
            Wake::Insertion(result) => {
                self.handle_insertion(result);
                None
            }
            Wake::Retired => {
                self.microphone = Microphone::Free;
                None
            }
            Wake::Timer => self.gesture.tick(now()),
        };
        self.handle_action(action)
    }

    fn handle_input(&mut self, input: Input) -> Option<Action> {
        match input {
            Input::Press if self.desktop_ready => self.gesture.press(now()),
            Input::Toggle if self.desktop_ready => self.gesture.toggle(now()),
            Input::Press | Input::Toggle => None,
            Input::Release => self.gesture.release(now()),
            Input::Lock => self.gesture.lock(),
            Input::Cancel => self.gesture.cancel(),
            Input::DesktopReady { shortcut, cancel } => {
                self.desktop_ready = true;
                self.shortcut = shortcut.into();
                self.cancel_shortcut = cancel.into();
                None
            }
            Input::Unavailable(_) => None,
        }
    }

    fn configure(&mut self) {
        let next = self.changes.borrow_and_update().clone();
        let reload = self.config.speech_changed(&next);
        self.config = next;
        self.gesture.cancel();
        if reload {
            self.worker.replace(&self.ports, &self.config, false);
        }
        self.abandon();
        if matches!(self.worker, Worker::Unavailable) {
            self.worker.replace(&self.ports, &self.config, false);
        }
        self.feedback = Feedback::Idle;
        self.message = if self.worker.is_ready() {
            "Settings applied"
        } else {
            LOADING
        }
        .into();
    }

    // This lookup is the only way callback messages acquire a current capture.
    // A matching identity alone cannot make late audio valid after completion.
    fn take_capture(&mut self, id: SessionId) -> Option<Session> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.id == id && session.capturing())
        {
            self.session.take()
        } else {
            None
        }
    }

    fn handle_audio(&mut self, event: Event) {
        match event {
            Event::Ready(id) => {
                if let Some(session) = self.take_capture(id) {
                    self.session = Some(session.ready(self.gesture.state == State::Processing));
                }
            }
            Event::Level(id, level) => {
                if let Some(mut session) = self.take_capture(id) {
                    session.level = level;
                    session.meter_tick = session.meter_tick.wrapping_add(1);
                    self.session = Some(session);
                }
            }
            Event::AudioDone(id, result) => {
                let Some(mut session) = self.take_capture(id) else {
                    return;
                };
                self.microphone.retire();
                self.gesture.finish();
                match result {
                    Ok(Some(wav)) => {
                        session.stage = Stage::AwaitingWorker(wav);
                        self.session = Some(session);
                    }
                    Ok(None) => {
                        session.permit.revoke();
                        self.gesture.complete();
                        self.feedback = Feedback::Empty;
                    }
                    Err(error) => {
                        session.permit.revoke();
                        self.fail(error.to_string());
                    }
                }
            }
        }
    }

    fn handle_worker(&mut self, completion: Completion<P::Speech>) {
        self.worker = Worker::Unavailable;
        let inference = matches!(completion, Completion::Transcribed(_));
        let result = match completion {
            Completion::Loaded(result) => result.map(|result| result.map(|worker| (worker, None))),
            Completion::Transcribed(result) => result,
        };
        match result {
            Ok(Ok((worker, text))) => {
                self.worker = Worker::Ready(worker);
                if !inference && self.feedback != Feedback::Error && self.message == LOADING {
                    self.message.clear();
                }
                // Only the current owned handle can produce this completion.
                // Cancellation moves it into recovery before another session.
                if inference
                    && let Some(text) = text
                    && let Some(session) = self.session.as_mut()
                    && matches!(session.stage, Stage::Transcribing)
                {
                    if text.is_empty() {
                        self.abandon();
                        self.gesture.complete();
                        self.feedback = Feedback::Empty;
                    } else {
                        let work = self.ports.insert(
                            text,
                            session.permit.clone(),
                            self.config.preserve_clipboard,
                        );
                        session.stage = Stage::Inserting(tokio::spawn(work));
                    }
                }
            }
            failure => {
                let message = match failure {
                    Ok(Err(error)) => error.to_string(),
                    _ => "Transcription worker stopped unexpectedly. Try again.".into(),
                };
                if !inference
                    && self
                        .session
                        .as_ref()
                        .is_some_and(|session| !matches!(session.stage, Stage::AwaitingWorker(_)))
                {
                    // Warmup failure does not complete a stopping gesture. The
                    // owned microphone must finish before a retry may start.
                    self.message = message;
                } else {
                    self.fail(message);
                }
                if inference {
                    self.worker.replace(&self.ports, &self.config, false);
                }
            }
        }
    }

    fn handle_insertion(
        &mut self,
        result: Result<anyhow::Result<Inserted>, tokio::task::JoinError>,
    ) {
        // The task was joined by wait(); its handle no longer owns pending work.
        if let Some(session) = self.session.take() {
            session.permit.revoke();
        }
        match result {
            Ok(Ok(Inserted::Sent)) => self.feedback = Feedback::Done,
            Ok(Ok(Inserted::Cancelled)) => self.feedback = Feedback::Cancelled,
            Ok(Ok(Inserted::Unavailable(message) | Inserted::Copied(message))) => {
                self.feedback = Feedback::Error;
                self.message = message.into();
            }
            _ => {
                self.feedback = Feedback::Error;
                self.message = "Could not submit text. Check desktop permissions and clipboard access, then try again.".into();
            }
        }
        self.gesture.complete();
    }

    fn handle_action(&mut self, action: Option<Action>) -> bool {
        match action {
            Some(Action::Start) => {
                let Some(permit) = self.gate.begin() else {
                    return false;
                };
                let Some(id) = self.latest_id.next() else {
                    return false;
                };
                self.latest_id = id;
                self.latest_started = now();
                self.feedback = Feedback::Idle;
                self.message.clear();
                self.session = Some(Session {
                    id,
                    started: self.latest_started,
                    permit,
                    level: 0.0,
                    meter_tick: 0,
                    stage: Stage::Queued {
                        microphone: self.config.microphone.clone(),
                    },
                });
            }
            Some(Action::Finish) => {
                if let Some(recording) = self.microphone.recording() {
                    recording.finish();
                }
                if let Some(session) = self.session.take() {
                    self.session = Some(session.finish());
                }
            }
            Some(Action::Cancel) => {
                self.abandon();
                if matches!(self.worker, Worker::Unavailable) {
                    self.worker.replace(&self.ports, &self.config, false);
                }
                self.feedback = Feedback::Cancelled;
            }
            _ => {}
        }
        true
    }

    // Moving owned work here invalidates authority without awaiting any OS call.
    fn abandon(&mut self) {
        if let Some(session) = self.session.take() {
            session.permit.revoke();
            match session.stage {
                Stage::Opening | Stage::Recording | Stage::Stopping => {
                    self.microphone.retire();
                }
                Stage::Inserting(task) => self.retiring_insertions.push(task),
                Stage::AwaitingWorker(mut wav) => wav.fill(0),
                Stage::Transcribing if self.worker.is_transcribing() => {
                    self.worker.replace(&self.ports, &self.config, true);
                }
                Stage::Queued { .. } | Stage::Transcribing => {}
            }
        }
    }

    fn fail(&mut self, message: String) {
        self.gesture.cancel();
        self.abandon();
        self.feedback = Feedback::Error;
        self.message = message;
    }

    async fn advance(&mut self) {
        if !self.gate.is_closed()
            && self.microphone.free()
            && self
                .session
                .as_ref()
                .is_some_and(|session| matches!(session.stage, Stage::Queued { .. }))
            && let Some(mut session) = self.session.take()
        {
            let Stage::Queued { microphone } = session.stage else {
                return;
            };
            match self
                .ports
                .record(session.id, microphone, self.events.clone())
            {
                Ok(recording) => {
                    if self.gesture.state == State::Processing {
                        recording.finish();
                    }
                    self.microphone = Microphone::Open(recording);
                    session.stage = Stage::Opening;
                    self.session = Some(session);
                }
                Err(error) => {
                    session.permit.revoke();
                    self.fail(format!("Could not start microphone worker: {error}"));
                }
            }
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| matches!(session.stage, Stage::AwaitingWorker(_)))
        {
            if matches!(self.worker, Worker::Unavailable) {
                self.message = LOADING.into();
                self.worker.replace(&self.ports, &self.config, false);
            }
            if self.worker.is_ready()
                && let Some(mut session) = self.session.take()
            {
                let Stage::AwaitingWorker(wav) = session.stage else {
                    return;
                };
                session.stage = match self.worker.transcribe(wav, self.config.language.clone()) {
                    Ok(()) => Stage::Transcribing,
                    Err(wav) => Stage::AwaitingWorker(wav),
                };
                self.session = Some(session);
            }
        }
        // Reap completed obsolete insertions without allowing their result to
        // change the current session. Cleanup tasks are never aborted on cancel.
        for index in (0..self.retiring_insertions.len()).rev() {
            if self.retiring_insertions[index].is_finished() {
                let _ = self.retiring_insertions.swap_remove(index).await;
            }
        }
    }

    fn publish(&self) {
        debug_assert_eq!(self.gesture.state == State::Idle, self.session.is_none());
        let snapshot = Snapshot {
            epoch: self.epoch,
            id: self.latest_id.get(),
            phase: self
                .session
                .as_ref()
                .map_or(self.feedback.phase(), Session::phase),
            hands_free: self.gesture.state == State::HandsFree,
            level: self.session.as_ref().map_or(0.0, |session| session.level),
            meter_tick: self
                .session
                .as_ref()
                .map_or(0, |session| session.meter_tick),
            started: self
                .session
                .as_ref()
                .map_or(self.latest_started, |session| session.started),
            message: self.message.clone(),
            model: self.worker.model(),
            desktop_ready: self.desktop_ready,
            shortcut: self.shortcut.clone(),
            cancel_shortcut: self.cancel_shortcut.clone(),
        };
        self.output.send_if_modified(|published| {
            if published.epoch != self.epoch || *published == snapshot {
                return false;
            }
            *published = snapshot;
            true
        });
    }

    async fn shutdown(mut self) {
        self.gate.close();
        self.audio.close();
        self.abandon();
        self.worker.request_stop();
        let insertions = async {
            for task in self.retiring_insertions {
                let _ = task.await;
            }
        };
        tokio::join!(self.worker.stop(), self.microphone.stop(), insertions);
    }
}
