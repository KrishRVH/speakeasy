use crate::{
    config::Config,
    ports::{Desktop, Ports, Recording, Speech},
};
use async_channel::Receiver;
use speakeasy_core::gesture::{Action, Gesture, State};
use speakeasy_platform::{Input, Inserted};
use std::{
    thread,
    time::{Duration, Instant},
};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Recording,
    Stopping,
    Processing,
    Done,
    Empty,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ModelState {
    Loading,
    Ready,
    #[default]
    Unavailable,
}

#[derive(Clone, PartialEq)]
pub struct Snapshot {
    pub id: u64,
    pub phase: Phase,
    pub hands_free: bool,
    pub level: f32,
    pub meter_tick: u64,
    pub started: Instant,
    pub message: String,
    pub model: ModelState,
    pub desktop_ready: bool,
    pub shortcut: std::sync::Arc<str>,
    pub cancel_shortcut: std::sync::Arc<str>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            id: 0,
            phase: Phase::Idle,
            hands_free: false,
            level: 0.0,
            meter_tick: 0,
            started: Instant::now(),
            message: String::new(),
            model: ModelState::Unavailable,
            desktop_ready: true,
            shortcut: speakeasy_platform::SHORTCUT.into(),
            cancel_shortcut: if cfg!(target_os = "linux") {
                "Ctrl + Super + Escape"
            } else {
                "Escape"
            }
            .into(),
        }
    }
}

// The loading notice clears once the first model is ready, unless replaced.
pub const LOADING: &str = "Loading local model…";

pub enum Event {
    Ready(u64),
    Level(u64, f32),
    AudioDone(u64, anyhow::Result<Option<Vec<u8>>>),
}
struct Job<W> {
    session: Option<u64>,
    cancel: Option<oneshot::Sender<()>>,
    task: JoinHandle<anyhow::Result<(W, Option<String>)>>,
}

#[cfg(test)]
mod fixture;

pub struct Runtime {
    pub input: speakeasy_platform::InputSender,
    thread: Option<thread::JoinHandle<()>>,
    configuration: watch::Sender<Config>,
    stop: async_channel::Sender<()>,
    finished: async_channel::Receiver<()>,
}
impl Runtime {
    pub fn start(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
    ) -> anyhow::Result<(Self, speakeasy_platform::InputMonitor)> {
        let (sender, receiver) = async_channel::bounded(64);
        let input = speakeasy_platform::InputSender::new(sender);
        let (monitor, inserter) =
            speakeasy_platform::prepare(input.clone(), config.desktop_options())?;
        let runtime = Self::spawn(config, snapshots, Desktop { inserter }, input, receiver)?;
        Ok((runtime, monitor))
    }
    #[cfg(test)]
    fn start_with<P: Ports>(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
        ports: P,
    ) -> anyhow::Result<Self> {
        let (input, receiver) = async_channel::bounded(64);
        let input = speakeasy_platform::InputSender::new(input);
        Self::spawn(config, snapshots, ports, input, receiver)
    }
    fn spawn<P: Ports>(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
        ports: P,
        input: speakeasy_platform::InputSender,
        receiver: Receiver<Input>,
    ) -> anyhow::Result<Self> {
        let gate = input.clone();
        let (configuration, changes) = watch::channel(config);
        let (stop, stopping) = async_channel::bounded(1);
        let (finished_tx, finished) = async_channel::bounded(1);
        let thread = thread::Builder::new()
            .name("dictation".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        runtime.block_on(run(changes, receiver, snapshots, gate, stopping, ports));
                    }
                    Err(_) => {
                        snapshots.send_modify(|s| {
                            s.phase = Phase::Error;
                            s.message = "Could not start dictation worker".into();
                        });
                    }
                }
                let _ = finished_tx.try_send(());
            })?;
        Ok(Self {
            input,
            thread: Some(thread),
            configuration,
            stop,
            finished,
        })
    }
    pub fn is_running(&self) -> bool {
        !self.input.is_closed()
            && self
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
    }
    pub fn configure(&self, config: Config) {
        speakeasy_platform::deliver(&self.input, Input::Cancel);
        self.configuration.send_replace(config);
    }
    pub fn request_stop(&self) {
        // Wake the owner through a separate lane before closing input, so a
        // normal pause never reports shortcut failure or drains queued presses.
        let _ = self.stop.try_send(());
        self.input.close();
    }
    pub fn stopped(&self) -> impl std::future::Future<Output = ()> + use<> {
        let finished = self.finished.clone();
        async move {
            let _ = finished.recv().await;
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn warm<P: Ports>(ports: &P, config: &Config) -> Job<P::Speech> {
    let load = ports.load(config.clone());
    Job {
        session: None,
        cancel: None,
        task: tokio::spawn(async move { Ok((load.await?, None)) }),
    }
}

async fn cancel<P: Ports>(
    mut job: Job<P::Speech>,
    ports: &P,
    config: &Config,
) -> (Option<P::Speech>, Option<Job<P::Speech>>) {
    if config.use_gpu
        && let Some(signal) = job.cancel.take()
    {
        let _ = signal.send(());
    } else {
        job.task.abort();
    }
    match job.task.await {
        // Already completed: discard the text and keep the usable model.
        Ok(Ok((worker, Some(_)))) => (Some(worker), None),
        Ok(Ok((mut worker, None))) if config.use_gpu => {
            let load = ports.load(config.clone());
            let task = tokio::spawn(async move {
                // Request cancellation closes the socket. The inference barrier
                // runs off the input path and bounds native cooperative cleanup.
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(2), worker.idle()).await,
                    Ok(Ok(()))
                ) {
                    worker.stop().await;
                    return Ok((load.await?, None));
                }
                Ok((worker, None))
            });
            (
                None,
                Some(Job {
                    session: None,
                    cancel: None,
                    task,
                }),
            )
        }
        _ => (None, Some(warm(ports, config))),
    }
}

async fn run<P: Ports>(
    mut changes: watch::Receiver<Config>,
    input: Receiver<Input>,
    output: watch::Sender<Snapshot>,
    gate: speakeasy_platform::InputSender,
    stopping: Receiver<()>,
    ports: P,
) {
    let mut config = changes.borrow_and_update().clone();
    let (events, audio) = async_channel::bounded(64);
    let mut gesture = Gesture::default();
    let mut snapshot = Snapshot {
        desktop_ready: !ports.prepares_desktop(),
        model: ModelState::Loading,
        message: LOADING.into(),
        ..Snapshot::default()
    };
    output.send_replace(snapshot.clone());
    let mut capture: Option<P::Recording> = None;
    let mut server: Option<P::Speech> = None;
    let mut job = Some(warm(&ports, &config));
    let mut waiting_audio = None;
    let mut pending_text: Option<String> = None;
    let mut permit = None;
    let mut insertion: Option<(u64, JoinHandle<anyhow::Result<Inserted>>)> = None;
    let mut retiring_insertions = Vec::new();
    loop {
        let deadline = gesture.deadline();
        let timer = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at.into()).await,
                None => std::future::pending::<()>().await,
            }
        };
        let completion = async {
            match job.as_mut() {
                Some(job) => (job.session, (&mut job.task).await),
                None => std::future::pending().await,
            }
        };
        let inserted = async {
            match insertion.as_mut() {
                Some((id, task)) => (*id, task.await),
                None => std::future::pending().await,
            }
        };
        let mut action = None;
        tokio::select! {
            biased;
            _ = stopping.recv() => break,
            event = input.recv() => {
                let Ok(event) = event else {
                    snapshot.phase = Phase::Error;
                    snapshot.message =
                        "Shortcut monitoring stopped. Open Speakeasy and enable dictation again."
                            .into();
                    output.send_replace(snapshot.clone());
                    break;
                };
                action = match event {
                    Input::Press if snapshot.desktop_ready => gesture.press(Instant::now()),
                    Input::Press => None,
                    Input::Release => gesture.release(Instant::now()),
                    Input::Lock => gesture.lock(),
                    Input::Toggle if snapshot.desktop_ready => gesture.toggle(Instant::now()),
                    Input::Toggle => None,
                    Input::Cancel => gesture.cancel(),
                    Input::Quit => break,
                    Input::Unavailable(message) => {
                        snapshot.phase = Phase::Error;
                        snapshot.message = message;
                        output.send_replace(snapshot.clone());
                        break;
                    }
                    Input::DesktopReady { shortcut, cancel } => {
                        snapshot.desktop_ready = true;
                        snapshot.shortcut = shortcut.into();
                        snapshot.cancel_shortcut = cancel.into();
                        None
                    }
                };
            }
            changed = changes.changed() => {
                if changed.is_err() {
                    break;
                }
                let next = changes.borrow_and_update().clone();
                let reload = config.speech_changed(&next);
                config = next;
                gesture.cancel();
                capture.take();
                waiting_audio = None;
                pending_text = None;
                if let Some((_, task)) = insertion.take() {
                    retiring_insertions.push(task);
                }
                if reload && let Some(previous) = job.take() {
                    previous.task.abort();
                    if let Ok(Ok((worker, _))) = previous.task.await {
                        server = Some(worker);
                    }
                } else if job.as_ref().is_some_and(|job| job.session.is_some())
                    && let Some(previous) = job.take()
                {
                    (server, job) = cancel(previous, &ports, &config).await;
                }
                if reload && let Some(mut worker) = server.take() {
                    worker.stop().await;
                }
                if server.is_none() && job.is_none() {
                    job = Some(warm(&ports, &config));
                }
                snapshot.phase = Phase::Idle;
                snapshot.message = if server.is_none() {
                    LOADING
                } else {
                    "Settings applied"
                }
                .into();
            }
            event = audio.recv() => match event {
                Ok(Event::Ready(id)) if id == snapshot.id && capture.is_some() => {
                    if snapshot.phase == Phase::Starting {
                        snapshot.phase = if gesture.state == State::Processing {
                            Phase::Stopping
                        } else {
                            Phase::Recording
                        };
                    }
                }
                Ok(Event::Level(id, level)) if id == snapshot.id && capture.is_some() => {
                    snapshot.level = level;
                    snapshot.meter_tick = snapshot.meter_tick.wrapping_add(1);
                }
                Ok(Event::AudioDone(id, result)) if id == snapshot.id && capture.is_some() => {
                    capture.take();
                    gesture.finish();
                    match result {
                        Ok(Some(wav)) => {
                            snapshot.phase = Phase::Processing;
                            waiting_audio = Some(wav);
                        }
                        Ok(None) => {
                            gesture.complete();
                            snapshot.phase = Phase::Empty;
                        }
                        Err(error) => {
                            gesture.cancel();
                            snapshot.phase = Phase::Error;
                            snapshot.message = error.to_string();
                        }
                    }
                }
                _ => {}
            },
            result = completion => {
                job.take();
                let (session, result) = result;
                match result {
                    Ok(Ok((worker, text))) => {
                        server = Some(worker);
                        if session.is_none()
                            && snapshot.phase != Phase::Error
                            && snapshot.message == LOADING
                        {
                            snapshot.message.clear();
                        }
                        if let Some(text) = text.filter(|_| session == Some(snapshot.id)) {
                            if text.is_empty() {
                                gesture.complete();
                                snapshot.phase = Phase::Empty;
                            } else {
                                pending_text = Some(text);
                            }
                        }
                    }
                    failure => {
                        if gesture.state == State::Processing {
                            gesture.complete();
                        }
                        waiting_audio = None;
                        if capture.is_none() {
                            snapshot.phase = Phase::Error;
                        }
                        snapshot.message = match failure {
                            Ok(Err(error)) => error.to_string(),
                            _ => "Transcription worker stopped unexpectedly. Try again.".into(),
                        };
                        // A failed session lost its owned worker. Reload in the
                        // background; a failed warmup itself must never retry-loop.
                        if session.is_some() {
                            job = Some(warm(&ports, &config));
                        }
                    }
                }
            }
            (id, result) = inserted => {
                insertion.take();
                if id == snapshot.id && snapshot.phase == Phase::Processing {
                    match result {
                        Ok(Ok(Inserted::Sent)) => snapshot.phase = Phase::Done,
                        Ok(Ok(Inserted::Cancelled)) => snapshot.phase = Phase::Cancelled,
                        Ok(Ok(Inserted::Unavailable(message) | Inserted::Copied(message))) => {
                            snapshot.phase = Phase::Error;
                            snapshot.message = message.into();
                        }
                        _ => {
                            snapshot.phase = Phase::Error;
                            snapshot.message = "Could not submit text. Check desktop permissions and clipboard access, then try again.".into();
                        }
                    }
                    gesture.complete();
                }
            }
            _ = timer => {
                action = gesture.tick(Instant::now());
            }
        }
        match action {
            Some(Action::Start) => {
                permit = gate.begin();
                if permit.is_none() {
                    break;
                }
                let Some(id) = snapshot.id.checked_add(1) else {
                    break;
                };
                snapshot = Snapshot {
                    desktop_ready: snapshot.desktop_ready,
                    shortcut: snapshot.shortcut.clone(),
                    cancel_shortcut: snapshot.cancel_shortcut.clone(),
                    id,
                    phase: Phase::Starting,
                    started: Instant::now(),
                    ..Snapshot::default()
                };
                match ports.record(id, config.microphone.clone(), events.clone()) {
                    Ok(recording) => capture = Some(recording),
                    Err(_) => {
                        gesture.cancel();
                        snapshot.phase = Phase::Error;
                        snapshot.message = "Could not start microphone worker".into();
                    }
                }
            }
            Some(Action::Finish) => {
                if let Some(recording) = &capture {
                    recording.finish();
                    if snapshot.phase == Phase::Recording {
                        snapshot.phase = Phase::Stopping;
                    }
                }
            }
            Some(Action::Cancel) => {
                capture.take();
                waiting_audio = None;
                pending_text = None;
                if let Some((_, task)) = insertion.take() {
                    retiring_insertions.push(task);
                }
                // Model loading and recovery are independent of a recording.
                if job.as_ref().is_some_and(|job| job.session.is_some())
                    && let Some(previous) = job.take()
                {
                    (server, job) = cancel(previous, &ports, &config).await;
                }
                if server.is_none() && job.is_none() {
                    job = Some(warm(&ports, &config));
                }
                snapshot.phase = Phase::Cancelled;
            }
            _ => {}
        }
        if let Some(text) = pending_text.take()
            && let Some(permit) = permit.clone()
        {
            let work = ports.insert(text, permit, config.preserve_clipboard);
            insertion = Some((snapshot.id, tokio::spawn(work)));
        }
        retiring_insertions.retain(|task| !task.is_finished());
        if job.is_none()
            && let Some(wav) = waiting_audio.take()
        {
            let existing = server.take();
            let load = ports.load(config.clone());
            let language = config.language.clone();
            let (cancel, mut cancelled) = oneshot::channel();
            job = Some(Job {
                session: Some(snapshot.id),
                cancel: Some(cancel),
                task: tokio::spawn(async move {
                    let worker = match existing {
                        Some(worker) => worker,
                        None => tokio::select! {
                            biased;
                            _ = &mut cancelled => anyhow::bail!("Transcription cancelled"),
                            worker = load => worker?,
                        },
                    };
                    let text = tokio::select! {
                        biased;
                        _ = &mut cancelled => None,
                        result = worker.transcribe(wav, &language) => Some(result?),
                    };
                    Ok((worker, text))
                }),
            });
        }
        snapshot.hands_free = gesture.state == State::HandsFree;
        snapshot.model = if job.as_ref().is_some_and(|job| job.session.is_none()) {
            ModelState::Loading
        } else if server.is_some() || job.is_some() {
            ModelState::Ready
        } else {
            ModelState::Unavailable
        };
        output.send_if_modified(|published| {
            if *published == snapshot {
                return false;
            }
            *published = snapshot.clone();
            true
        });
    }
    gate.close();
    audio.close();
    capture.take();
    if let Some((_, task)) = insertion {
        retiring_insertions.push(task);
    }
    for task in retiring_insertions {
        let _ = task.await;
    }
    if let Some(job) = job {
        job.task.abort();
        let _ = job.task.await;
    }
    if let Some(mut worker) = server {
        worker.stop().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_channel::{Receiver, Sender};
    use tokio::sync::oneshot;

    type InsertControls = (Sender<speakeasy_platform::InsertPermit>, Receiver<()>);
    struct FakePorts {
        insertion: Option<InsertControls>,
        captures: Sender<(u64, Sender<Event>)>,
        jobs: Sender<(String, oneshot::Sender<anyhow::Result<String>>)>,
        pasted: Sender<String>,
        loads: Sender<()>,
        stop: Option<(Sender<()>, Receiver<()>)>,
        recovery: Option<Receiver<anyhow::Result<()>>>,
    }
    struct FakeRecording {
        id: u64,
        events: Sender<Event>,
    }
    impl Recording for FakeRecording {
        fn finish(&self) {
            assert!(
                self.events
                    .try_send(Event::AudioDone(self.id, Ok(Some(vec![1, 2]))))
                    .is_ok()
            );
        }
    }
    struct FakeSpeech {
        jobs: Sender<(String, oneshot::Sender<anyhow::Result<String>>)>,
        stop: Option<(Sender<()>, Receiver<()>)>,
        recovery: Option<Receiver<anyhow::Result<()>>>,
    }
    impl Speech for FakeSpeech {
        async fn transcribe(&self, _: Vec<u8>, language: &str) -> anyhow::Result<String> {
            let (reply, result) = oneshot::channel();
            self.jobs.send((language.to_owned(), reply)).await?;
            result.await?
        }
        async fn idle(&self) -> anyhow::Result<()> {
            match &self.recovery {
                Some(result) => result.recv().await?,
                None => Ok(()),
            }
        }
        async fn stop(&mut self) {
            if let Some((entered, release)) = &self.stop {
                let _ = entered.send(()).await;
                let _ = tokio::time::timeout(Duration::from_secs(2), release.recv()).await;
            }
        }
    }
    impl Ports for FakePorts {
        type Recording = FakeRecording;
        type Speech = FakeSpeech;
        fn record(
            &self,
            id: u64,
            _: Option<String>,
            events: Sender<Event>,
        ) -> anyhow::Result<FakeRecording> {
            self.captures.try_send((id, events.clone()))?;
            events.try_send(Event::Ready(id))?;
            Ok(FakeRecording { id, events })
        }
        fn load(
            &self,
            _: Config,
        ) -> impl std::future::Future<Output = anyhow::Result<FakeSpeech>> + Send + 'static
        {
            let jobs = self.jobs.clone();
            let loads = self.loads.clone();
            let stop = self.stop.clone();
            let recovery = self.recovery.clone();
            async move {
                loads.send(()).await?;
                Ok(FakeSpeech {
                    jobs,
                    stop,
                    recovery,
                })
            }
        }
        fn insert(
            &self,
            text: String,
            gate: speakeasy_platform::InsertPermit,
            _: bool,
        ) -> impl std::future::Future<Output = anyhow::Result<Inserted>> + Send + 'static {
            let pasted = self.pasted.clone();
            let insertion = self.insertion.clone();
            async move {
                if let Some((entered, release)) = insertion {
                    entered.send(gate.clone()).await?;
                    release.recv().await?;
                }
                if !gate.commit() {
                    return Ok(Inserted::Cancelled);
                }
                pasted.try_send(text)?;
                Ok(Inserted::Sent)
            }
        }
    }
    struct Harness {
        runtime: Runtime,
        updates: watch::Receiver<Snapshot>,
        captures: Receiver<(u64, Sender<Event>)>,
        jobs: Receiver<(String, oneshot::Sender<anyhow::Result<String>>)>,
        pasted: Receiver<String>,
        loads: Receiver<()>,
    }
    impl Harness {
        fn new() -> anyhow::Result<Self> {
            Self::with_controls(None, None)
        }
        fn with_controls(
            stop: Option<(Sender<()>, Receiver<()>)>,
            recovery: Option<Receiver<anyhow::Result<()>>>,
        ) -> anyhow::Result<Self> {
            Self::with_insertion(stop, recovery, None)
        }
        fn with_insertion(
            stop: Option<(Sender<()>, Receiver<()>)>,
            recovery: Option<Receiver<anyhow::Result<()>>>,
            insertion: Option<InsertControls>,
        ) -> anyhow::Result<Self> {
            let (capture_tx, captures) = async_channel::bounded(8);
            let (job_tx, jobs) = async_channel::bounded(8);
            let (paste_tx, pasted) = async_channel::bounded(8);
            let (load_tx, loads) = async_channel::bounded(8);
            let (output, updates) = watch::channel(Snapshot::default());
            let runtime = Runtime::start_with(
                Config::default(),
                output,
                FakePorts {
                    insertion,
                    captures: capture_tx,
                    jobs: job_tx,
                    pasted: paste_tx,
                    loads: load_tx,
                    stop,
                    recovery,
                },
            )?;
            Ok(Self {
                runtime,
                updates,
                captures,
                jobs,
                pasted,
                loads,
            })
        }
        fn input(&self, input: Input) {
            speakeasy_platform::deliver(&self.runtime.input, input);
        }
        async fn phase(&mut self, wanted: Phase) -> anyhow::Result<()> {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if self.updates.borrow_and_update().phase == wanted {
                        return Ok::<_, anyhow::Error>(());
                    }
                    self.updates.changed().await?;
                }
            })
            .await??;
            Ok(())
        }
        async fn start(&mut self) -> anyhow::Result<(u64, Sender<Event>)> {
            self.input(Input::Toggle);
            self.phase(Phase::Recording).await?;
            receive(&self.captures).await
        }
        async fn finish(&mut self) -> anyhow::Result<oneshot::Sender<anyhow::Result<String>>> {
            self.input(Input::Toggle);
            let (language, reply) = receive(&self.jobs).await?;
            assert_eq!(language, self.runtime.configuration.borrow().language);
            Ok(reply)
        }
    }
    async fn receive<T>(receiver: &Receiver<T>) -> anyhow::Result<T> {
        Ok(tokio::time::timeout(Duration::from_secs(2), receiver.recv()).await??)
    }

    #[tokio::test]
    async fn preparing_insertion_cancels_without_blocking_a_new_recording() -> anyhow::Result<()> {
        let (entered, preparing) = async_channel::bounded(2);
        let (release, waiting) = async_channel::bounded(2);
        let mut h = Harness::with_insertion(None, None, Some((entered, waiting)))?;
        h.start().await?;
        assert!(h.finish().await?.send(Ok("old fixture".into())).is_ok());
        let old = receive(&preparing).await?;
        h.input(Input::Cancel);
        h.phase(Phase::Cancelled).await?;
        h.start().await?;
        assert!(!old.active(), "A newer capture authorized stale insertion");
        release.send(()).await?;
        assert!(h.finish().await?.send(Ok("new fixture".into())).is_ok());
        let next = receive(&preparing).await?;
        assert!(next.active());
        assert!(!old.commit());
        release.send(()).await?;
        assert_eq!(receive(&h.pasted).await?, "new fixture");
        h.phase(Phase::Done).await?;
        assert!(h.pasted.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_revokes_and_owns_pending_insertion_cleanup() -> anyhow::Result<()> {
        let (entered, preparing) = async_channel::bounded(1);
        let (release, waiting) = async_channel::bounded(1);
        let mut h = Harness::with_insertion(None, None, Some((entered, waiting)))?;
        h.start().await?;
        assert!(h.finish().await?.send(Ok("fixture".into())).is_ok());
        let permit = receive(&preparing).await?;
        h.runtime.request_stop();
        assert!(!permit.active());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), h.runtime.stopped())
                .await
                .is_err()
        );
        release.send(()).await?;
        tokio::time::timeout(Duration::from_secs(2), h.runtime.stopped()).await?;
        assert!(h.pasted.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn silence_and_empty_recognition_never_report_submission() -> anyhow::Result<()> {
        let mut h = Harness::new()?;
        let (id, events) = h.start().await?;
        events.send(Event::AudioDone(id, Ok(None))).await?;
        h.phase(Phase::Empty).await?;
        assert!(h.jobs.try_recv().is_err());
        assert!(h.pasted.try_recv().is_err());
        h.start().await?;
        assert!(h.finish().await?.send(Ok(String::new())).is_ok());
        h.phase(Phase::Empty).await?;
        assert!(h.pasted.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn dictation_inserts_once_and_request_preferences_keep_the_warm_worker()
    -> anyhow::Result<()> {
        let mut h = Harness::new()?;
        receive(&h.loads).await?;
        h.start().await?;
        assert!(h.finish().await?.send(Ok("fixture one".into())).is_ok());
        assert_eq!(receive(&h.pasted).await?, "fixture one");
        h.phase(Phase::Done).await?;
        h.input(Input::Cancel);
        let config = Config {
            reduced_motion: true,
            preserve_clipboard: true,
            language: "auto".into(),
            ..Config::default()
        };
        h.runtime.configure(config);
        h.phase(Phase::Idle).await?;
        h.start().await?;
        assert!(h.finish().await?.send(Ok("fixture two".into())).is_ok());
        assert_eq!(receive(&h.pasted).await?, "fixture two");
        h.phase(Phase::Done).await?;
        assert!(
            h.loads.try_recv().is_err(),
            "Request preferences reloaded the model"
        );
        assert!(h.pasted.try_recv().is_err(), "A result was submitted twice");
        let mut config = h.runtime.configuration.borrow().clone();
        config.engine = crate::config::Engine::Parakeet;
        h.runtime.configure(config);
        receive(&h.loads).await?;
        h.start().await?;
        assert!(h.finish().await?.send(Ok("new engine".into())).is_ok());
        assert_eq!(receive(&h.pasted).await?, "new engine");
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_inference_and_late_audio_cannot_affect_the_next_session()
    -> anyhow::Result<()> {
        let mut h = Harness::new()?;
        receive(&h.loads).await?;
        let (old_id, old_events) = h.start().await?;
        let old_reply = h.finish().await?;
        h.input(Input::Cancel);
        h.phase(Phase::Cancelled).await?;
        assert!(
            old_reply.send(Ok("discard this".into())).is_err(),
            "Cancelled inference stayed alive"
        );
        let (new_id, _) = h.start().await?;
        assert!(new_id > old_id);
        old_events
            .send(Event::AudioDone(
                old_id,
                Err(anyhow::anyhow!("old device failed")),
            ))
            .await?;
        assert!(h.finish().await?.send(Ok("new session".into())).is_ok());
        assert_eq!(receive(&h.pasted).await?, "new session");
        h.phase(Phase::Done).await?;
        assert!(
            h.loads.try_recv().is_err(),
            "Cancelled GPU inference reloaded a healthy model"
        );
        assert!(h.pasted.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn failures_recover_the_worker_and_hook_loss_stops_without_inserting()
    -> anyhow::Result<()> {
        let mut h = Harness::new()?;
        receive(&h.loads).await?;
        let (id, events) = h.start().await?;
        events
            .send(Event::AudioDone(
                id,
                Err(anyhow::anyhow!("Microphone disconnected")),
            ))
            .await?;
        h.phase(Phase::Error).await?;
        assert!(
            h.updates
                .borrow()
                .message
                .contains("Microphone disconnected")
        );
        h.start().await?;
        assert!(
            h.finish()
                .await?
                .send(Err(anyhow::anyhow!("Fixture request failed")))
                .is_ok()
        );
        h.phase(Phase::Error).await?;
        receive(&h.loads).await?;
        assert!(
            h.updates
                .borrow()
                .message
                .contains("Fixture request failed")
        );
        h.start().await?;
        h.runtime.input.close();
        h.phase(Phase::Error).await?;
        assert!(!h.runtime.is_running());
        assert!(h.pasted.try_recv().is_err());
        Ok(())
    }
    #[tokio::test]
    async fn failed_recovery_stops_worker_before_loading_replacement() -> anyhow::Result<()> {
        let (entered, stopping) = async_channel::bounded(2);
        let (release, waiting) = async_channel::bounded(2);
        let (recover, recovery) = async_channel::bounded(1);
        let mut h = Harness::with_controls(Some((entered, waiting)), Some(recovery))?;
        receive(&h.loads).await?;
        h.start().await?;
        let old_reply = h.finish().await?;
        h.input(Input::Cancel);
        h.phase(Phase::Cancelled).await?;
        assert!(old_reply.send(Ok("discard this".into())).is_err());
        recover
            .send(Err(anyhow::anyhow!("Recovery failed")))
            .await?;
        receive(&stopping).await?;
        assert!(
            h.loads.try_recv().is_err(),
            "Replacement overlapped worker cleanup"
        );
        h.start().await?;
        h.input(Input::Toggle);
        h.phase(Phase::Processing).await?;
        assert!(
            h.jobs.try_recv().is_err(),
            "Inference began before recovery finished"
        );
        release.send(()).await?;
        receive(&h.loads).await?;
        let (_, reply) = receive(&h.jobs).await?;
        assert!(reply.send(Ok("after recovery".into())).is_ok());
        assert_eq!(receive(&h.pasted).await?, "after recovery");
        h.phase(Phase::Done).await?;
        assert!(h.pasted.try_recv().is_err());
        release.send(()).await?; // Permit final native cleanup when the harness drops.
        Ok(())
    }

    #[tokio::test]
    async fn stop_cancels_input_without_waiting_for_native_cleanup() -> anyhow::Result<()> {
        let (entered, stopping) = async_channel::bounded(1);
        let (release, waiting) = async_channel::bounded(1);
        let mut h = Harness::with_controls(Some((entered, waiting)), None)?;
        h.start().await?;
        assert!(h.finish().await?.send(Ok("fixture".into())).is_ok());
        receive(&h.pasted).await?;
        h.phase(Phase::Done).await?;

        h.start().await?;
        h.runtime.request_stop();
        receive(&stopping).await?;
        assert!(h.runtime.input.is_closed());
        assert!(!h.runtime.input.active());
        let mut stopped = std::pin::pin!(h.runtime.stopped());
        assert!(
            std::future::Future::poll(
                stopped.as_mut(),
                &mut std::task::Context::from_waker(std::task::Waker::noop()),
            )
            .is_pending(),
            "Completion was reported before native cleanup finished"
        );
        release.send(()).await?;
        tokio::time::timeout(Duration::from_secs(2), stopped).await?;
        assert!(h.pasted.try_recv().is_err());
        Ok(())
    }

    // A focused diagnostic using the real owner and fake side effects. No timing
    // assertion: scheduler noise must not turn performance profiling into a gate.
    #[tokio::test]
    #[ignore = "Reports controller reply-to-insertion latency with fake devices"]
    async fn profile_ready_text_latency() -> anyhow::Result<()> {
        let mut h = Harness::new()?;
        receive(&h.loads).await?;
        let mut samples = Vec::with_capacity(100);
        for _ in 0..100 {
            h.start().await?;
            let reply = h.finish().await?;
            let started = Instant::now();
            assert!(reply.send(Ok("fixture".into())).is_ok());
            receive(&h.pasted).await?;
            samples.push(started.elapsed().as_micros());
            h.phase(Phase::Done).await?;
        }
        samples.sort_unstable();
        eprintln!(
            "Ready text -> fake insertion: median={}us p95={}us n={}",
            samples[50],
            samples[95],
            samples.len()
        );
        Ok(())
    }
}
