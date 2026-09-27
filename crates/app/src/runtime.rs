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
    Cancelled,
    Error,
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
        }
    }
}

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
    pub fn start(config: Config, snapshots: watch::Sender<Snapshot>) -> anyhow::Result<Self> {
        Self::start_with(config, snapshots, Desktop)
    }
    fn start_with<P: Ports>(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
        ports: P,
    ) -> anyhow::Result<Self> {
        let (input, receiver) = async_channel::bounded(64);
        let input = speakeasy_platform::InputSender::new(input);
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
    let mut snapshot = Snapshot::default();
    let mut capture: Option<P::Recording> = None;
    let mut server: Option<P::Speech> = None;
    let mut job = Some(warm(&ports, &config));
    let mut waiting_audio = None;
    let mut pending_text: Option<(String, Instant)> = None;
    loop {
        let deadline = if pending_text.is_some() {
            Some(Instant::now() + Duration::from_millis(10))
        } else {
            gesture.deadline()
        };
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
        let mut action = None;
        tokio::select! {
            biased;
            _ = stopping.recv() => break,
            event = input.recv() => {
                let Ok(event) = event else {
                    snapshot.phase = Phase::Error;
                    snapshot.message = "Shortcut monitoring stopped. Open Speakeasy and enable dictation again.".into();
                    output.send_replace(snapshot.clone());
                    break;
                };
                action = match event {
                    Input::Press => gesture.press(Instant::now()),
                    Input::Release => gesture.release(Instant::now()),
                    Input::Toggle => gesture.toggle(Instant::now()),
                    Input::Cancel => gesture.cancel(),
                    Input::Quit => break,
                };
            }
            changed = changes.changed() => {
                if changed.is_err() { break; }
                let next = changes.borrow_and_update().clone();
                let reload = config.speech_changed(&next);
                config = next;
                gesture.cancel();
                capture.take();
                waiting_audio = None;
                pending_text = None;
                if reload && let Some(previous) = job.take() {
                    previous.task.abort();
                    if let Ok(Ok((worker, _))) = previous.task.await { server = Some(worker); }
                } else if job.as_ref().is_some_and(|job| job.session.is_some()) && let Some(previous) = job.take() {
                    (server, job) = cancel(previous, &ports, &config).await;
                }
                if reload && let Some(mut worker) = server.take() { worker.stop().await; }
                if server.is_none() && job.is_none() {
                    job = Some(warm(&ports, &config));
                }
                snapshot.phase = Phase::Idle;
                snapshot.message = if server.is_none() { "Loading local model…" } else { "Settings applied" }.into();
            }
            event = audio.recv() => {
                match event {
                    Ok(Event::Ready(id)) if id == snapshot.id && capture.is_some() => {
                        if snapshot.phase == Phase::Starting { snapshot.phase = if gesture.state == State::Processing { Phase::Stopping } else { Phase::Recording }; }
                    }
                    Ok(Event::Level(id, level)) if id == snapshot.id && capture.is_some() => {
                        snapshot.level = level;
                        snapshot.meter_tick = snapshot.meter_tick.wrapping_add(1);
                    },
                    Ok(Event::AudioDone(id, result)) if id == snapshot.id && capture.is_some() => {
                        capture.take();
                        gesture.finish();
                        match result {
                            Ok(Some(wav)) => { snapshot.phase = Phase::Processing; waiting_audio = Some(wav); }
                            Ok(None) => { gesture.complete(); snapshot.phase = Phase::Done; }
                            Err(error) => { gesture.cancel(); snapshot.phase = Phase::Error; snapshot.message = error.to_string(); }
                        }
                    }
                    _ => {}
                }
            }
            result = completion => {
                job.take();
                let (session, result) = result;
                match result {
                    Ok(Ok((worker, text))) => {
                        server = Some(worker);
                        if session.is_none() && snapshot.phase != Phase::Error && snapshot.message == "Loading local model…" { snapshot.message.clear(); }
                        if let Some(text) = text.filter(|_| session == Some(snapshot.id)) {
                            if text.is_empty() { gesture.complete(); snapshot.phase = Phase::Done; }
                            else { pending_text = Some((text, Instant::now())); }
                        }
                    }
                    failure => {
                        if gesture.state == State::Processing { gesture.complete(); }
                        waiting_audio = None;
                        if capture.is_none() { snapshot.phase = Phase::Error; }
                        snapshot.message = match failure { Ok(Err(error)) => error.to_string(), _ => "Transcription worker stopped unexpectedly. Try again.".into() };
                        // A failed session lost its owned worker. Reload in the
                        // background; a failed warmup itself must never retry-loop.
                        if session.is_some() { job = Some(warm(&ports, &config)); }
                    }
                }
            }
            _ = timer => {
                action = gesture.tick(Instant::now());
            }
        }
        match action {
            Some(Action::Start) => {
                gate.begin();
                let Some(id) = snapshot.id.checked_add(1) else {
                    break;
                };
                snapshot = Snapshot {
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
        // Ready text can be submitted in this owner turn. Only held modifiers
        // need the retry timer; cancellation actions above still clear the result.
        if let Some((_, since)) = &pending_text
            && (!ports.modifiers_down() || since.elapsed() >= Duration::from_millis(800))
        {
            if input.is_closed() {
                break;
            }
            if let Some((text, _)) = pending_text.take() {
                match ports.insert(&text, &gate, config.preserve_clipboard) {
                    Ok(Inserted::Sent) => snapshot.phase = Phase::Done,
                    Ok(Inserted::Cancelled) => snapshot.phase = Phase::Cancelled,
                    Ok(Inserted::Unavailable(message)) => {
                        snapshot.phase = Phase::Error;
                        snapshot.message = message.into();
                    }
                    Ok(Inserted::Copied(message)) => {
                        snapshot.phase = Phase::Error;
                        snapshot.message = message.into();
                    }
                    Err(_) => {
                        snapshot.phase = Phase::Error;
                        snapshot.message = "Could not access the clipboard. Close the app holding it and try again.".into();
                    }
                }
                // Submission has committed. Later Escape cannot claim to undo it.
                gesture.complete();
            }
        }
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
        output.send_if_modified(|published| {
            if *published == snapshot {
                return false;
            }
            *published = snapshot.clone();
            true
        });
    }
    audio.close();
    capture.take();
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

    struct FakePorts {
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
        fn modifiers_down(&self) -> bool {
            false
        }
        fn insert(
            &self,
            text: &str,
            gate: &speakeasy_platform::InputSender,
            _: bool,
        ) -> anyhow::Result<Inserted> {
            if !gate.commit() {
                return Ok(Inserted::Cancelled);
            }
            self.pasted.try_send(text.to_owned())?;
            Ok(Inserted::Sent)
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
            let (capture_tx, captures) = async_channel::bounded(8);
            let (job_tx, jobs) = async_channel::bounded(8);
            let (paste_tx, pasted) = async_channel::bounded(8);
            let (load_tx, loads) = async_channel::bounded(8);
            let (output, updates) = watch::channel(Snapshot::default());
            let runtime = Runtime::start_with(
                Config::default(),
                output,
                FakePorts {
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
        assert!(!h.runtime.input.commit());
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
