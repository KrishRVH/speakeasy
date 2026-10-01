//! Drives the production owner through fake ports, either on its own thread or on the test's paused
//! clock, where deadlines are exact.

use std::time::Duration;

use anyhow::{anyhow, bail, ensure};
use speakeasy_platform::{Delivery, InsertPermit, Inserted};
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};

use super::*;
use crate::{
    config::Engine,
    ports::{Recording, Speech},
};

const PATIENCE: Duration = Duration::from_secs(2);

type Reply = oneshot::Sender<anyhow::Result<String>>;
type TranscriptionJob = (String, Reply);
type OpenedCapture = (SessionId, Sender<CaptureEvent>);

/// Where a fake pauses for the test: it reports entry, then waits for release.
#[derive(Clone)]
struct Gate<T> {
    entered: Sender<T>,
    release: Receiver<()>,
}

impl<T: Send + Sync + 'static> Gate<T> {
    async fn pass(&self, value: T) -> anyhow::Result<()> {
        self.entered.send(value).await?;
        self.release.recv().await?;
        Ok(())
    }

    #[expect(
        clippy::let_underscore_must_use,
        reason = "Harness disposal closes cleanup gates after a failed assertion; owned teardown must still finish"
    )]
    async fn pass_during_cleanup(&self, value: T) {
        let _ = self.entered.send(value).await;
        let _ = self.release.recv().await;
    }
}

/// The test's side of a [`Gate`].
struct GateControl<T> {
    entered: Receiver<T>,
    release: Sender<()>,
}

impl<T> GateControl<T> {
    async fn reached(&self) -> anyhow::Result<T> {
        receive(&self.entered).await
    }

    async fn release(&self) -> anyhow::Result<()> {
        Ok(self.release.send(()).await?)
    }
}

/// Fake devices, speech, and insertion. Each optional gate or result lane pauses or steers one side
/// effect.
struct FakePorts {
    desktop_pending: bool,
    record_error: Option<&'static str>,
    delayed_finish: bool,
    load_results: Option<Receiver<anyhow::Result<()>>>,
    recovery: Option<Receiver<anyhow::Result<()>>>,
    retirement: Option<Gate<()>>,
    stop: Option<Gate<()>>,
    insertion: Option<Gate<InsertPermit>>,
    captures: Sender<OpenedCapture>,
    jobs: Sender<TranscriptionJob>,
    pasted: Sender<String>,
    loads: Sender<()>,
}

/// What the fakes report back to the test.
struct Reports {
    captures: Receiver<OpenedCapture>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    loads: Receiver<()>,
}

impl FakePorts {
    fn new() -> (Self, Reports) {
        let (captures, opened) = async_channel::bounded(8);
        let (jobs, requested) = async_channel::bounded(8);
        let (pasted, submitted) = async_channel::bounded(8);
        let (loads, loaded) = async_channel::bounded(8);
        let ports = Self {
            desktop_pending: false,
            record_error: None,
            delayed_finish: false,
            load_results: None,
            recovery: None,
            retirement: None,
            stop: None,
            insertion: None,
            captures,
            jobs,
            pasted,
            loads,
        };
        let reports = Reports {
            captures: opened,
            jobs: requested,
            pasted: submitted,
            loads: loaded,
        };
        (ports, reports)
    }

    fn cleanup_gates(&self) -> CleanupGates {
        let releases = [&self.retirement, &self.stop]
            .into_iter()
            .flatten()
            .map(|gate| gate.release.clone());
        let insertion = self.insertion.iter().map(|gate| gate.release.clone());
        CleanupGates(releases.chain(insertion).collect())
    }
}

impl Ports for FakePorts {
    type Recording = FakeRecording;
    type Speech = FakeSpeech;

    fn prepares_desktop(&self) -> bool {
        self.desktop_pending
    }

    fn record(
        &self,
        id: SessionId,
        _: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<FakeRecording> {
        if let Some(error) = self.record_error {
            bail!(error);
        }
        self.captures.try_send((id, events.clone()))?;
        events.try_send(CaptureEvent::Ready(id))?;
        Ok(FakeRecording {
            id,
            events,
            delayed_finish: self.delayed_finish,
            retirement: self.retirement.clone(),
        })
    }

    fn load(
        &self,
        _: Config,
        mut cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<FakeSpeech>> + Send + 'static {
        let load_results = self.load_results.clone();
        let speech = FakeSpeech {
            jobs: self.jobs.clone(),
            stop: self.stop.clone(),
            recovery: self.recovery.clone(),
        };
        let loads = self.loads.clone();
        async move {
            loads.send(()).await?;
            if let Some(results) = load_results {
                tokio::select! {
                    biased;
                    _ = cancelled.changed() => bail!("Fake model load cancelled"),
                    result = results.recv() => result??,
                }
            }
            Ok(speech)
        }
    }

    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        _: Delivery,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let pasted = self.pasted.clone();
        let insertion = self.insertion.clone();
        async move {
            if let Some(gate) = insertion {
                gate.pass(permit.clone()).await?;
            }
            if !permit.commit() {
                return Ok(Inserted::Cancelled);
            }
            pasted.try_send(text)?;
            Ok(Inserted::Sent)
        }
    }
}

struct FakeRecording {
    id: SessionId,
    events: Sender<CaptureEvent>,
    delayed_finish: bool,
    retirement: Option<Gate<()>>,
}

impl Recording for FakeRecording {
    fn finish(&self) {
        if self.delayed_finish {
            return;
        }
        let finished = CaptureEvent::Finished(self.id, Ok(Some(vec![1, 2])));
        self.events
            .try_send(finished)
            .expect("capture lane has room for fake audio");
    }

    async fn retire(self) {
        if let Some(gate) = self.retirement {
            gate.pass_during_cleanup(()).await;
        }
    }
}

struct FakeSpeech {
    jobs: Sender<TranscriptionJob>,
    stop: Option<Gate<()>>,
    recovery: Option<Receiver<anyhow::Result<()>>>,
}

impl Speech for FakeSpeech {
    async fn transcribe(&self, _: Vec<u8>, language: &str) -> anyhow::Result<String> {
        let (reply, result) = oneshot::channel();
        self.jobs.send((language.to_owned(), reply)).await?;
        result.await?
    }

    async fn probe_with_silence(&self) -> anyhow::Result<()> {
        match &self.recovery {
            Some(result) => result.recv().await?,
            None => Ok(()),
        }
    }

    async fn stop(&mut self) {
        if let Some(gate) = &self.stop {
            gate.pass_during_cleanup(()).await;
        }
    }
}

/// Fake cleanup gates closed when a harness is dropped, so a failed assertion cannot leave owned
/// teardown waiting for a release that never comes.
struct CleanupGates(Vec<Receiver<()>>);

impl Drop for CleanupGates {
    fn drop(&mut self) {
        for gate in &self.0 {
            gate.close();
        }
    }
}

/// The production controls and owner, plus the test's view of the fakes. Gates close before the
/// owner is stopped and released.
struct Harness<Owner = OwnedThread> {
    _cleanup: CleanupGates,
    runtime: Runtime<Owner>,
    output: watch::Sender<Snapshot>,
    updates: watch::Receiver<Snapshot>,
    captures: Receiver<OpenedCapture>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    loads: Receiver<()>,
}

impl Harness {
    fn new() -> anyhow::Result<Self> {
        Self::with_ports(Config::default(), |_| {})
    }

    fn with_ports(config: Config, customize: impl FnOnce(&mut FakePorts)) -> anyhow::Result<Self> {
        Self::host(config, customize, Runtime::start_with)
    }
}

impl Harness<JoinHandle<()>> {
    /// Runs the owner on this test's executor, so `tokio::time::advance` drives its deadlines.
    fn paused(customize: impl FnOnce(&mut FakePorts)) -> anyhow::Result<Self> {
        Self::host(Config::default(), customize, |config, output, ports| {
            let (input, inputs) = input_lane();
            Runtime::host(config, output, ports, input, inputs, |wiring| {
                Ok(tokio::spawn(owner::run(wiring)))
            })
        })
    }

    async fn close(mut self) -> anyhow::Result<()> {
        self.runtime.request_stop();
        (&mut self.runtime.owner).await?;
        Ok(())
    }
}

// Sync keeps the futures of the `&self` helpers Send (clippy::future_not_send).
impl<Owner: Sync> Harness<Owner> {
    fn host(
        config: Config,
        customize: impl FnOnce(&mut FakePorts),
        start: impl FnOnce(Config, watch::Sender<Snapshot>, FakePorts) -> anyhow::Result<Runtime<Owner>>,
    ) -> anyhow::Result<Self> {
        let (mut ports, reports) = FakePorts::new();
        customize(&mut ports);
        let cleanup = ports.cleanup_gates();
        let (output, updates) = watch::channel(Snapshot::default());
        let runtime = start(config, output.clone(), ports)?;
        Ok(Self {
            _cleanup: cleanup,
            runtime,
            output,
            updates,
            captures: reports.captures,
            jobs: reports.jobs,
            pasted: reports.pasted,
            loads: reports.loads,
        })
    }

    fn input(&self, input: Input) {
        self.runtime.input.deliver(input);
    }

    async fn observe(&mut self, predicate: impl FnMut(&Snapshot) -> bool) -> anyhow::Result<()> {
        timeout(PATIENCE, self.updates.wait_for(predicate)).await??;
        Ok(())
    }

    async fn phase(&mut self, wanted: Phase) -> anyhow::Result<()> {
        self.observe(|snapshot| snapshot.phase == wanted).await
    }

    fn snapshot(&self) -> Snapshot {
        self.updates.borrow().clone()
    }

    async fn start(&mut self) -> anyhow::Result<OpenedCapture> {
        self.input(Input::Toggle);
        self.phase(Phase::Recording).await?;
        receive(&self.captures).await
    }

    /// Finishes the capture and returns the transcription request, which carries the configured
    /// language.
    async fn finish(&self) -> anyhow::Result<Reply> {
        self.input(Input::Toggle);
        let (language, reply) = receive(&self.jobs).await?;
        assert_eq!(language, self.runtime.configuration.borrow().language);
        Ok(reply)
    }

    async fn finish_as(&self, text: &str) -> anyhow::Result<()> {
        answer(self.finish().await?, text)
    }

    async fn transcribe_next_as(&self, text: &str) -> anyhow::Result<()> {
        let (_, reply) = receive(&self.jobs).await?;
        answer(reply, text)
    }
}

fn gate<T>(capacity: usize) -> (Gate<T>, GateControl<T>) {
    let (entered, entries) = async_channel::bounded(capacity);
    let (releases, release) = async_channel::bounded(capacity);
    let gate = Gate { entered, release };
    let control = GateControl {
        entered: entries,
        release: releases,
    };
    (gate, control)
}

async fn receive<T>(receiver: &Receiver<T>) -> anyhow::Result<T> {
    Ok(timeout(PATIENCE, receiver.recv()).await??)
}

fn answer(reply: Reply, text: &str) -> anyhow::Result<()> {
    ensure!(
        reply.send(Ok(text.to_owned())).is_ok(),
        "The transcription request was abandoned"
    );
    Ok(())
}

async fn still_pending(future: impl Future) -> bool {
    timeout(Duration::from_millis(30), future).await.is_err()
}

fn level_is(level: f32) -> impl FnMut(&Snapshot) -> bool {
    move |snapshot| snapshot.level.to_bits() == level.to_bits()
}

#[tokio::test(start_paused = true)]
async fn desktop_readiness_blocks_capture_until_native_startup_completes() -> anyhow::Result<()> {
    let mut h = Harness::paused(|ports| ports.desktop_pending = true)?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.input(Input::Toggle);
    h.input(Input::DesktopReady {
        shortcut: "fixture shortcut".into(),
        cancel: "fixture cancel".into(),
    });
    h.observe(|snapshot| snapshot.desktop_ready).await?;
    assert!(h.captures.try_recv().is_err());
    assert_eq!(&*h.snapshot().shortcut, "fixture shortcut");
    assert_eq!(&*h.snapshot().cancel_shortcut, "fixture cancel");
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn held_shortcut_release_finishes_through_the_owner() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    tokio::time::advance(Duration::from_millis(250)).await;
    h.input(Input::Release);
    h.transcribe_next_as("").await?;
    h.phase(Phase::Empty).await?;
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn locking_a_hold_keeps_the_same_capture_until_the_next_press() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Lock);
    h.observe(|snapshot| snapshot.hands_free).await?;
    h.input(Input::Release);
    events.send(CaptureEvent::Level(id, 0.25)).await?;
    h.observe(level_is(0.25)).await?;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(h.jobs.try_recv().is_err());
    assert!(h.captures.try_recv().is_err());
    h.input(Input::Press);
    receive(&h.jobs).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn double_tap_uses_one_capture_and_single_tap_obeys_its_deadline() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    tokio::time::advance(Duration::from_millis(50)).await;
    h.input(Input::Release);
    events.send(CaptureEvent::Level(id, 0.25)).await?;
    h.observe(level_is(0.25)).await?;
    tokio::time::advance(Duration::from_millis(100)).await;
    h.input(Input::Press);
    h.observe(|snapshot| snapshot.hands_free).await?;
    assert!(h.captures.try_recv().is_err());
    assert!(h.jobs.try_recv().is_err());
    h.input(Input::Release);
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;

    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    tokio::time::advance(Duration::from_millis(50)).await;
    h.input(Input::Release);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    events.send(CaptureEvent::Level(id, 0.5)).await?;
    h.observe(level_is(0.5)).await?;
    tokio::time::advance(Duration::from_millis(299)).await;
    assert!(h.jobs.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    receive(&h.jobs).await?;
    assert_eq!(tokio::time::Instant::now(), deadline);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn owner_finishes_capture_at_the_five_minute_deadline() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    h.start().await?;
    let limit = Duration::from_secs(300);
    let deadline = tokio::time::Instant::now() + limit;
    tokio::time::advance(limit.saturating_sub(Duration::from_millis(1))).await;
    assert!(h.jobs.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    receive(&h.jobs).await?;
    assert_eq!(tokio::time::Instant::now(), deadline);
    assert!(h.captures.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn failed_capture_start_preserves_its_reason_and_allows_shutdown() -> anyhow::Result<()> {
    let mut h = Harness::paused(|ports| ports.record_error = Some("thread resources exhausted"))?;
    h.input(Input::Press);
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("thread resources exhausted"));
    assert!(h.captures.try_recv().is_err());
    h.close().await
}

#[tokio::test]
async fn cancelled_capture_retires_before_retry_and_pause_waits_for_it() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
    })?;
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    retiring.reached().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Starting).await?;
    assert!(
        h.captures.try_recv().is_err(),
        "Retry overlapped a retiring microphone"
    );
    retiring.release().await?;
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.runtime.request_stop();
    retiring.reached().await?;
    assert!(still_pending(h.runtime.stopped()).await);
    retiring.release().await?;
    timeout(PATIENCE, h.runtime.stopped()).await?;
    Ok(())
}

#[tokio::test]
async fn pause_during_a_queued_capture_never_opens_the_new_device() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
    })?;
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    retiring.reached().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Starting).await?;
    h.runtime.request_stop();
    assert!(still_pending(h.runtime.stopped()).await);
    retiring.release().await?;
    timeout(PATIENCE, h.runtime.stopped()).await?;
    assert!(h.captures.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn completed_audio_does_not_allow_retry_before_device_teardown() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
    })?;
    h.start().await?;
    h.finish_as("fixture").await?;
    retiring.reached().await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    h.input(Input::Toggle);
    h.phase(Phase::Starting).await?;
    assert!(h.captures.try_recv().is_err());
    retiring.release().await?;
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.runtime.request_stop();
    retiring.reached().await?;
    retiring.release().await?;
    timeout(PATIENCE, h.runtime.stopped()).await?;
    Ok(())
}

#[tokio::test]
async fn pause_starts_capture_and_model_cleanup_together() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(1);
    let (stop, stopping) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
        ports.stop = Some(stop);
    })?;
    receive(&h.loads).await?;
    h.start().await?;
    h.runtime.request_stop();
    retiring.reached().await?;
    stopping.reached().await?;
    assert!(still_pending(h.runtime.stopped()).await);
    retiring.release().await?;
    stopping.release().await?;
    timeout(PATIENCE, h.runtime.stopped()).await?;
    Ok(())
}

#[tokio::test]
async fn cpu_cancellation_reaps_before_replacement_without_blocking_input() -> anyhow::Result<()> {
    let (stop, stopping) = gate(2);
    let config = Config {
        use_gpu: false,
        ..Config::default()
    };
    let mut h = Harness::with_ports(config, |ports| ports.stop = Some(stop))?;
    receive(&h.loads).await?;
    h.start().await?;
    let mut old_reply = h.finish().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    stopping.reached().await?;
    timeout(PATIENCE, old_reply.closed()).await?;
    assert!(answer(old_reply, "stale fixture").is_err());
    assert!(
        h.loads.try_recv().is_err(),
        "Replacement started before old worker exit"
    );
    h.start().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Processing).await?;
    assert!(h.jobs.try_recv().is_err());
    stopping.release().await?;
    receive(&h.loads).await?;
    h.transcribe_next_as("current fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "current fixture");
    h.phase(Phase::Done).await?;
    // Pre-release the stop gate, so the shutdown stop passes without harness cleanup.
    stopping.release().await?;
    Ok(())
}

#[tokio::test]
async fn preparing_insertion_cancels_without_blocking_a_new_recording() -> anyhow::Result<()> {
    let (insertion, preparing) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.insertion = Some(insertion);
    })?;
    h.start().await?;
    h.finish_as("old fixture").await?;
    let old = preparing.reached().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.start().await?;
    assert!(!old.active(), "A newer capture authorized stale insertion");
    preparing.release().await?;
    h.finish_as("new fixture").await?;
    let next = preparing.reached().await?;
    assert!(next.active());
    assert!(!old.commit());
    preparing.release().await?;
    assert_eq!(receive(&h.pasted).await?, "new fixture");
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn shutdown_revokes_and_owns_pending_insertion_cleanup() -> anyhow::Result<()> {
    let (insertion, preparing) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.insertion = Some(insertion);
    })?;
    h.start().await?;
    h.finish_as("fixture").await?;
    let permit = preparing.reached().await?;
    h.runtime.request_stop();
    assert!(!permit.active());
    assert!(still_pending(h.runtime.stopped()).await);
    preparing.release().await?;
    timeout(PATIENCE, h.runtime.stopped()).await?;
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn silence_and_empty_recognition_never_report_submission() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, events) = h.start().await?;
    events.send(CaptureEvent::Finished(id, Ok(None))).await?;
    h.phase(Phase::Empty).await?;
    assert!(h.jobs.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    h.start().await?;
    h.finish_as("").await?;
    h.phase(Phase::Empty).await?;
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn dictation_inserts_once_and_request_preferences_keep_the_warm_worker() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    h.start().await?;
    h.finish_as("fixture one").await?;
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
    h.finish_as("fixture two").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture two");
    h.phase(Phase::Done).await?;
    assert!(
        h.loads.try_recv().is_err(),
        "Request preferences reloaded the model"
    );
    assert!(h.pasted.try_recv().is_err(), "A result was submitted twice");
    let mut config = h.runtime.configuration.borrow().clone();
    config.engine = Engine::Parakeet;
    h.runtime.configure(config);
    receive(&h.loads).await?;
    h.start().await?;
    h.finish_as("new engine").await?;
    assert_eq!(receive(&h.pasted).await?, "new engine");
    Ok(())
}

#[tokio::test]
async fn cancelled_inference_and_late_audio_cannot_affect_the_next_session() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    let (old_id, old_events) = h.start().await?;
    let mut old_reply = h.finish().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    timeout(PATIENCE, old_reply.closed()).await?;
    assert!(
        answer(old_reply, "discard this").is_err(),
        "Cancelled inference stayed alive"
    );
    let (new_id, _) = h.start().await?;
    assert_ne!(new_id, old_id);
    let late_failure = CaptureEvent::Finished(old_id, Err(anyhow!("old device failed")));
    old_events.send(late_failure).await?;
    h.finish_as("new session").await?;
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
async fn failures_recover_the_worker_and_hook_loss_stops_without_inserting() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    let (id, events) = h.start().await?;
    let disconnected = CaptureEvent::Finished(id, Err(anyhow!("Microphone disconnected")));
    events.send(disconnected).await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Microphone disconnected"));
    h.start().await?;
    let reply = h.finish().await?;
    assert!(reply.send(Err(anyhow!("Fixture request failed"))).is_ok());
    h.phase(Phase::Error).await?;
    receive(&h.loads).await?;
    assert!(h.snapshot().message.contains("Fixture request failed"));
    h.start().await?;
    h.runtime.input.close();
    h.phase(Phase::Error).await?;
    assert!(!h.runtime.is_running());
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn failed_recovery_stops_worker_before_loading_replacement() -> anyhow::Result<()> {
    let (stop, stopping) = gate(2);
    let (recover, recovery) = async_channel::bounded(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.stop = Some(stop);
        ports.recovery = Some(recovery);
    })?;
    receive(&h.loads).await?;
    h.start().await?;
    let mut old_reply = h.finish().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    timeout(PATIENCE, old_reply.closed()).await?;
    assert!(answer(old_reply, "discard this").is_err());
    recover.send(Err(anyhow!("Recovery failed"))).await?;
    stopping.reached().await?;
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
    stopping.release().await?;
    receive(&h.loads).await?;
    h.transcribe_next_as("after recovery").await?;
    assert_eq!(receive(&h.pasted).await?, "after recovery");
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    // Pre-release the stop gate, so the shutdown stop passes without harness cleanup.
    stopping.release().await?;
    Ok(())
}

#[tokio::test]
async fn stop_cancels_input_without_waiting_for_native_cleanup() -> anyhow::Result<()> {
    let (stop, stopping) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| ports.stop = Some(stop))?;
    h.start().await?;
    h.finish_as("fixture").await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;

    h.start().await?;
    h.runtime.request_stop();
    stopping.reached().await?;
    assert!(h.runtime.input.is_closed());
    assert!(!h.runtime.input.active());
    assert!(
        still_pending(h.runtime.stopped()).await,
        "Completion was reported before native cleanup finished"
    );
    stopping.release().await?;
    timeout(PATIENCE, h.runtime.stopped()).await?;
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn warmup_failure_while_stopping_retains_capture_and_retries_on_demand() -> anyhow::Result<()>
{
    let (results, loading) = async_channel::bounded(2);
    let mut h = Harness::paused(|ports| {
        ports.load_results = Some(loading);
        ports.delayed_finish = true;
    })?;
    receive(&h.loads).await?;
    let (id, events) = h.start().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Stopping).await?;
    results.send(Err(anyhow!("Model warmup failed"))).await?;
    h.observe(|snapshot| snapshot.message == "Model warmup failed")
        .await?;
    assert_eq!(h.snapshot().phase, Phase::Stopping);
    h.input(Input::Press);
    events.send(CaptureEvent::Level(id, 0.4)).await?;
    h.observe(level_is(0.4)).await?;
    assert!(h.captures.try_recv().is_err());
    assert!(h.loads.try_recv().is_err(), "Failed warmup must not loop");
    events
        .send(CaptureEvent::Finished(id, Ok(Some(vec![1, 2]))))
        .await?;
    receive(&h.loads).await?;
    results.send(Ok(())).await?;
    h.transcribe_next_as("fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture");
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn loading_notice_clears_once_ready_and_applied_settings_are_confirmed() -> anyhow::Result<()>
{
    let (results, loading) = async_channel::bounded(1);
    let mut h = Harness::paused(|ports| ports.load_results = Some(loading))?;
    receive(&h.loads).await?;
    h.observe(|snapshot| snapshot.message == LOADING).await?;
    results.send(Ok(())).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    assert_eq!(h.snapshot().message, "");
    h.runtime.configure(Config::default());
    h.observe(|snapshot| snapshot.message == "Settings applied")
        .await?;
    assert_eq!(h.snapshot().phase, Phase::Idle);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn audio_awaiting_an_unavailable_worker_shows_loading_until_it_is_ready() -> anyhow::Result<()>
{
    let (results, loading) = async_channel::bounded(2);
    let mut h = Harness::paused(|ports| ports.load_results = Some(loading))?;
    receive(&h.loads).await?;
    results.send(Err(anyhow!("Initial load failed"))).await?;
    h.phase(Phase::Error).await?;
    assert_eq!(h.snapshot().message, "Initial load failed");
    h.start().await?;
    assert_eq!(h.snapshot().message, "");
    h.input(Input::Toggle);
    receive(&h.loads).await?;
    h.observe(|snapshot| snapshot.message == LOADING).await?;
    assert_eq!(h.snapshot().phase, Phase::Processing);
    results.send(Ok(())).await?;
    h.transcribe_next_as("fixture").await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn configuration_revokes_insertion_authority_before_owner_observation() -> anyhow::Result<()>
{
    let mut h = Harness::paused(|_| {})?;
    h.start().await?;
    assert!(h.runtime.input.active());
    h.runtime.configure(Config::default());
    assert!(!h.runtime.input.active());
    h.phase(Phase::Idle).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn retired_publisher_cannot_overwrite_shell_lifecycle_notice() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    h.start().await?;
    h.output.send_modify(|snapshot| {
        snapshot.epoch += 1;
        snapshot.phase = Phase::Idle;
        snapshot.message = "Pausing dictation…".into();
    });
    h.input(Input::Toggle);
    h.transcribe_next_as("fixture").await?;
    receive(&h.pasted).await?;
    let updates = h.updates.clone();
    h.close().await?;
    assert_eq!(updates.borrow().message, "Pausing dictation…");
    assert_eq!(updates.borrow().phase, Phase::Idle);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn delayed_owner_start_cannot_claim_a_newer_shell_publisher() -> anyhow::Result<()> {
    let h = Harness::paused(|_| {})?;
    h.output.send_modify(|snapshot| {
        snapshot.epoch += 1;
        snapshot.message = "Dictation paused".into();
    });
    let updates = h.updates.clone();
    h.close().await?;
    assert_eq!(updates.borrow().message, "Dictation paused");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn recovery_failure_keeps_a_queued_dictation_until_microphone_retirement()
-> anyhow::Result<()> {
    let (results, loading) = async_channel::bounded(4);
    let (retirement, retiring) = gate(4);
    let mut h = Harness::paused(|ports| {
        ports.load_results = Some(loading);
        ports.retirement = Some(retirement);
    })?;
    receive(&h.loads).await?;
    h.start().await?;
    results.send(Err(anyhow!("Initial warmup failed"))).await?;
    h.observe(|snapshot| snapshot.message == "Initial warmup failed")
        .await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    retiring.reached().await?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.phase(Phase::Starting).await?;
    results.send(Err(anyhow!("Recovery failed"))).await?;
    h.observe(|snapshot| snapshot.message == "Recovery failed")
        .await?;
    assert_eq!(h.snapshot().phase, Phase::Starting);
    assert!(h.captures.try_recv().is_err());
    retiring.release().await?;
    receive(&h.captures).await?;
    h.phase(Phase::Recording).await?;
    h.input(Input::Toggle);
    receive(&h.loads).await?;
    results.send(Ok(())).await?;
    h.transcribe_next_as("fixture").await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    retiring.reached().await?;
    retiring.release().await?;
    h.close().await
}

// Scheduler noise must not turn performance profiling into a gate, so this only reports.
#[tokio::test]
#[ignore = "Reports owner reply-to-insertion latency with fake devices"]
async fn profile_ready_text_latency() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    let mut samples = Vec::with_capacity(100);
    for _ in 0..100 {
        h.start().await?;
        let reply = h.finish().await?;
        let started = Instant::now();
        answer(reply, "fixture")?;
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
