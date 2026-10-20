//! Drives the production owner through fake ports, either on its own thread or on the test's paused
//! clock, where deadlines are exact.

use std::{path::PathBuf, time::Duration};

use anyhow::{anyhow, bail, ensure};
use speakeasy_platform::{Delivery, InsertPermit, Inserted};
use speakeasy_show::{Image, Journal, Moment, Narration, Size};
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};

use super::*;
use crate::{
    config::Engine,
    ports::{Footage, Published, Recorded, Recording, Speech},
    theme::Theme,
};

const PATIENCE: Duration = Duration::from_secs(2);

type Reply = oneshot::Sender<anyhow::Result<String>>;
type TranscriptionJob = (String, Reply);
type OpenedCapture = (SessionId, Sender<CaptureEvent>);
type OpenedShow = (SessionId, Sender<ShowEvent>);

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
    /// Each insertion takes the next queued result once it commits, or reports `Sent`.
    insert_results: Option<Receiver<Inserted>>,
    show_error: Option<&'static str>,
    screen_retirement: Option<Gate<()>>,
    publication: Option<Gate<()>>,
    publish_error: Option<&'static str>,
    captures: Sender<OpenedCapture>,
    shows: Sender<OpenedShow>,
    jobs: Sender<TranscriptionJob>,
    pasted: Sender<String>,
    published: Sender<Recorded>,
    loads: Sender<()>,
}

/// What the fakes report back to the test.
struct Reports {
    captures: Receiver<OpenedCapture>,
    shows: Receiver<OpenedShow>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    published: Receiver<Recorded>,
    loads: Receiver<()>,
}

impl FakePorts {
    fn new() -> (Self, Reports) {
        let (captures, opened) = async_channel::bounded(8);
        let (shows, opened_shows) = async_channel::bounded(8);
        let (jobs, requested) = async_channel::bounded(8);
        let (pasted, submitted) = async_channel::bounded(8);
        let (published, written) = async_channel::bounded(8);
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
            insert_results: None,
            show_error: None,
            screen_retirement: None,
            publication: None,
            publish_error: None,
            captures,
            shows,
            jobs,
            pasted,
            published,
            loads,
        };
        let reports = Reports {
            captures: opened,
            shows: opened_shows,
            jobs: requested,
            pasted: submitted,
            published: written,
            loads: loaded,
        };
        (ports, reports)
    }

    fn cleanup_gates(&self) -> CleanupGates {
        let releases = [&self.retirement, &self.screen_retirement, &self.stop]
            .into_iter()
            .flatten()
            .map(|gate| gate.release.clone());
        let insertion = self.insertion.iter().map(|gate| gate.release.clone());
        let publication = self.publication.iter().map(|gate| gate.release.clone());
        CleanupGates(releases.chain(insertion).chain(publication).collect())
    }
}

impl Ports for FakePorts {
    type Recording = FakeRecording;
    type Show = FakeShow;
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

    fn show(
        &self,
        id: SessionId,
        _: Instant,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<FakeShow> {
        if let Some(error) = self.show_error {
            bail!(error);
        }
        self.shows.try_send((id, events.clone()))?;
        Ok(FakeShow {
            id,
            events,
            retirement: self.screen_retirement.clone(),
        })
    }

    fn publish(
        &self,
        recorded: Recorded,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        let published = self.published.clone();
        let publication = self.publication.clone();
        let publish_error = self.publish_error;
        async move {
            if let Some(gate) = publication {
                gate.pass(()).await?;
            }
            if let Some(error) = publish_error {
                bail!(error);
            }
            if !permit.commit() {
                return Ok(Published::Cancelled);
            }
            published.try_send(recorded)?;
            Ok(Published::Copied(PathBuf::from("fake show")))
        }
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
        let results = self.insert_results.clone();
        async move {
            if let Some(gate) = insertion {
                gate.pass(permit.clone()).await?;
            }
            if !permit.commit() {
                return Ok(Inserted::Cancelled);
            }
            let result = results
                .and_then(|results| results.try_recv().ok())
                .unwrap_or(Inserted::Sent);
            if result == Inserted::Sent {
                pasted.try_send(text)?;
            }
            Ok(result)
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
        let finished = CaptureEvent::Finished(
            self.id,
            Ok(Some(Sealed {
                wav: vec![1, 2],
                trimmed: Duration::ZERO,
            })),
        );
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

/// Reports one gray frame as its footage when finished.
struct FakeShow {
    id: SessionId,
    events: Sender<ShowEvent>,
    retirement: Option<Gate<()>>,
}

impl Recording for FakeShow {
    fn finish(&self) {
        self.events
            .try_send(ShowEvent::Finished(self.id, Ok(footage())))
            .expect("show lane has room for fake footage");
    }

    async fn retire(self) {
        if let Some(gate) = self.retirement {
            gate.pass_during_cleanup(()).await;
        }
    }
}

fn footage() -> Footage {
    let frame = Image::from_bgra(2, 2, vec![128; 16]).expect("a 2×2 frame");
    let mut journal = Journal::new();
    journal.record(Moment::START, frame);
    Footage {
        surface: Size {
            width: 1000.0,
            height: 1000.0,
        },
        ended: Moment::from_millis(1_000),
        journal,
        strokes: Vec::new(),
        actions: Vec::new(),
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

    async fn transcribe_words(
        &self,
        wav: Vec<u8>,
        language: &str,
    ) -> anyhow::Result<speakeasy_show::Narration> {
        self.transcribe(wav, language)
            .await
            .map(speakeasy_show::Narration::Text)
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
    levels: watch::Receiver<Meter>,
    captures: Receiver<OpenedCapture>,
    shows: Receiver<OpenedShow>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    published: Receiver<Recorded>,
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
        Self::host(
            Config::default(),
            customize,
            |config, output, meter, ports| {
                let (input, inputs) = input_lane();
                Runtime::host(config, output, meter, ports, input, inputs, |wiring| {
                    Ok(tokio::spawn(owner::run(wiring)))
                })
            },
        )
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
        start: impl FnOnce(
            Config,
            watch::Sender<Snapshot>,
            watch::Sender<Meter>,
            FakePorts,
        ) -> anyhow::Result<Runtime<Owner>>,
    ) -> anyhow::Result<Self> {
        let (mut ports, reports) = FakePorts::new();
        customize(&mut ports);
        let cleanup = ports.cleanup_gates();
        // A restarted shell's epoch, so a level tagged with the default epoch fails `observe_level`.
        let (output, updates) = watch::channel(Snapshot {
            epoch: 1,
            ..Snapshot::default()
        });
        let (meter, levels) = watch::channel(Meter::default());
        let runtime = start(config, output.clone(), meter, ports)?;
        Ok(Self {
            _cleanup: cleanup,
            runtime,
            output,
            updates,
            levels,
            captures: reports.captures,
            shows: reports.shows,
            jobs: reports.jobs,
            pasted: reports.pasted,
            published: reports.published,
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

    /// Waits until the meter shows exactly `level`, which proves the owner handled every input
    /// that outranked the level report, then checks that the level names the epoch and session
    /// the pill matches it against.
    async fn observe_level(&mut self, level: f32) -> anyhow::Result<()> {
        let meter = *timeout(
            PATIENCE,
            self.levels
                .wait_for(|meter| meter.level.to_bits() == level.to_bits()),
        )
        .await??;
        let snapshot = self.snapshot();
        assert_eq!(
            (meter.epoch, meter.session),
            (snapshot.epoch, snapshot.id),
            "The level is not tagged with the recording it measured"
        );
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
    h.observe_level(0.25).await?;
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
    h.observe_level(0.25).await?;
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
    h.observe_level(0.5).await?;
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
    for text in ["", "\t\r\n", "Um, uh. UM!"] {
        h.start().await?;
        h.finish_as(text).await?;
        h.phase(Phase::Empty).await?;
        assert!(h.pasted.try_recv().is_err());
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn copied_text_is_a_notice_that_carries_its_remedy_until_the_next_paste() -> anyhow::Result<()>
{
    const REMEDY: &str = "Text copied. Release your shortcut keys and paste into your editor.";
    let (results, queued) = async_channel::bounded(1);
    results.try_send(Inserted::Copied(REMEDY))?;
    let mut h = Harness::paused(|ports| ports.insert_results = Some(queued))?;
    h.start().await?;
    h.finish_as("fixture").await?;
    h.phase(Phase::Copied).await?;
    assert_eq!(h.snapshot().message, REMEDY);
    assert!(h.pasted.try_recv().is_err());
    h.start().await?;
    h.finish_as("pasted fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "pasted fixture");
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    h.close().await
}

#[tokio::test]
async fn saved_cleanup_opt_out_keeps_words_and_worker() -> anyhow::Result<()> {
    let mut config = Config::default();
    let mut h = Harness::with_ports(config.clone(), |_| {})?;
    receive(&h.loads).await?;
    h.start().await?;
    h.finish_as("Um, speak uh clearly.").await?;
    assert_eq!(receive(&h.pasted).await?, "speak clearly.");
    h.phase(Phase::Done).await?;
    config.remove_fillers = false;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    Config::save_changes(
        &[&Config::default()],
        &config,
        &ValidConfig::assume(config.clone()),
        &path,
    )?;
    h.runtime
        .configure(ValidConfig::assume(Config::read(&path)?));
    h.input(Input::Cancel);
    h.phase(Phase::Idle).await?;
    h.start().await?;
    h.finish_as("\nUm, speak uh literally.\n").await?;
    assert_eq!(receive(&h.pasted).await?, "Um, speak uh literally.");
    h.phase(Phase::Done).await?;
    assert!(
        h.loads.try_recv().is_err(),
        "Changing cleanup reloaded the model"
    );
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
    h.runtime.configure(ValidConfig::assume(config));
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
    h.runtime.configure(ValidConfig::assume(config));
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

#[tokio::test(start_paused = true)]
async fn levels_after_a_capture_ends_never_reach_the_meter() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    let (old_id, old_events) = h.start().await?;
    old_events.send(CaptureEvent::Level(old_id, 0.25)).await?;
    h.observe_level(0.25).await?;
    old_events
        .send(CaptureEvent::Finished(old_id, Ok(None)))
        .await?;
    h.phase(Phase::Empty).await?;
    // The capture lane is ordered, so each phase below is published after the level before it.
    old_events.send(CaptureEvent::Level(old_id, 0.5)).await?;
    let (new_id, events) = h.start().await?;
    events.send(CaptureEvent::Level(old_id, 0.75)).await?;
    events
        .send(CaptureEvent::Finished(new_id, Ok(None)))
        .await?;
    h.phase(Phase::Empty).await?;
    assert!(
        !h.levels.has_changed()?,
        "A level from an ended capture reached the meter"
    );
    h.close().await
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

#[tokio::test(start_paused = true)]
async fn monitor_failure_behind_a_queued_press_still_reports_its_remedy() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.runtime.input.fail("X".into());
    h.observe(|snapshot| snapshot.phase == Phase::Error && snapshot.message == "X")
        .await?;
    assert!(h.captures.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_lane_closed_behind_a_queued_press_still_reports_monitoring_stopped() -> anyhow::Result<()>
{
    let mut h = Harness::paused(|_| {})?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.runtime.input.close();
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Shortcut monitoring stopped"));
    h.close().await
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
    h.observe_level(0.4).await?;
    assert!(h.captures.try_recv().is_err());
    assert!(h.loads.try_recv().is_err(), "Failed warmup must not loop");
    events
        .send(CaptureEvent::Finished(
            id,
            Ok(Some(Sealed {
                wav: vec![1, 2],
                trimmed: Duration::ZERO,
            })),
        ))
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
    h.runtime.configure(ValidConfig::assume(Config {
        remove_fillers: false,
        ..Config::default()
    }));
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
    h.runtime.configure(ValidConfig::assume(Config {
        remove_fillers: false,
        ..Config::default()
    }));
    assert!(!h.runtime.input.active());
    h.phase(Phase::Idle).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn presentation_settings_keep_the_active_session() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    h.start().await?;
    h.runtime.configure(ValidConfig::assume(Config {
        theme: Theme::default().next(),
        reduced_motion: true,
        ..Config::default()
    }));
    assert!(h.runtime.input.active());
    h.finish_as("fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture");
    h.phase(Phase::Done).await?;
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

const REPORT: &str = "Could not delete every expired Show";

#[tokio::test(start_paused = true)]
async fn an_idle_report_survives_model_load_and_desktop_readiness() -> anyhow::Result<()> {
    let (results, loading) = async_channel::bounded(1);
    let mut h = Harness::paused(|ports| {
        ports.load_results = Some(loading);
        ports.desktop_pending = true;
    })?;
    receive(&h.loads).await?;
    h.runtime
        .report(REPORT.into())
        .map_err(anyhow::Error::msg)?;
    h.phase(Phase::Error).await?;
    results.send(Ok(())).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    h.input(Input::DesktopReady {
        shortcut: "fixture shortcut".into(),
        cancel: "fixture cancel".into(),
    });
    h.observe(|snapshot| snapshot.desktop_ready).await?;
    assert_eq!(h.snapshot().phase, Phase::Error);
    assert_eq!(h.snapshot().message, REPORT);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_report_during_a_held_recording_is_dropped_and_the_text_still_pastes()
-> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.runtime
        .report(REPORT.into())
        .map_err(anyhow::Error::msg)?;
    // The report lane outranks capture, so the level arrives only after the report is handled.
    events.send(CaptureEvent::Level(id, 0.25)).await?;
    h.observe_level(0.25).await?;
    assert_eq!(h.snapshot().message, "", "The recording showed the report");
    tokio::time::advance(Duration::from_millis(250)).await;
    h.input(Input::Release);
    h.transcribe_next_as("fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture");
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    h.close().await
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

/// Holds the shortcut and presses S, returning the screen recorder's lane.
async fn start_show<Owner: Sync>(h: &mut Harness<Owner>) -> anyhow::Result<OpenedShow> {
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show);
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    receive(&h.shows).await
}

#[tokio::test(start_paused = true)]
async fn s_during_a_hold_turns_the_recording_into_a_show_and_publishes_it() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    start_show(&mut h).await?;
    // The Show is locked hands-free: releasing the shortcut keeps recording.
    h.input(Input::Release);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(h.jobs.try_recv().is_err());
    h.input(Input::Press);
    let (_, reply) = receive(&h.jobs).await?;
    answer(reply, "look here")?;
    let recorded = receive(&h.published).await?;
    assert_eq!(
        recorded.recording.narration,
        Narration::Text("look here".into())
    );
    assert_eq!(recorded.journal.samples().len(), 1);
    h.phase(Phase::Copied).await?;
    assert!(h.snapshot().message.contains("Show copied"));
    assert!(h.pasted.try_recv().is_err(), "A Show never pastes");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_silent_show_still_publishes_its_screen() -> anyhow::Result<()> {
    let mut h = Harness::paused(|ports| ports.delayed_finish = true)?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Show);
    receive(&h.shows).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    events.send(CaptureEvent::Finished(id, Ok(None))).await?;
    let recorded = receive(&h.published).await?;
    assert_eq!(recorded.recording.narration, Narration::default());
    assert!(h.jobs.try_recv().is_err(), "Silence is not transcribed");
    h.phase(Phase::Copied).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_show_publishes_nothing_and_frees_the_screen() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    start_show(&mut h).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(h.published.try_recv().is_err());
    // The next Show opens a new screen recorder.
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.close().await
}

#[tokio::test]
async fn cancelling_while_publishing_leaves_no_trace() -> anyhow::Result<()> {
    let (publication, publishing) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publication = Some(publication);
    })?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("this broke").await?;
    publishing.reached().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    publishing.release().await?;
    assert!(still_pending(h.published.recv()).await);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_show_that_cannot_be_published_explains_why() -> anyhow::Result<()> {
    let mut h = Harness::paused(|ports| ports.publish_error = Some("disk full"))?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("look").await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("disk full"));
    h.close().await
}

#[tokio::test]
async fn shutdown_awaits_a_retiring_screen() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.screen_retirement = Some(retirement);
    })?;
    start_show(&mut h).await?;
    h.runtime.request_stop();
    retiring.reached().await?;
    assert!(still_pending(h.runtime.stopped()).await);
    retiring.release().await?;
    timeout(PATIENCE, h.runtime.stopped()).await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_revokes_and_owns_pending_publication_cleanup() -> anyhow::Result<()> {
    let (publication, publishing) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publication = Some(publication);
    })?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("fixture").await?;
    publishing.reached().await?;
    h.runtime.request_stop();
    assert!(still_pending(h.runtime.stopped()).await);
    publishing.release().await?;
    timeout(PATIENCE, h.runtime.stopped()).await?;
    assert!(
        h.published.try_recv().is_err(),
        "A revoked Show was published"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_cannot_be_recorded_explains_why() -> anyhow::Result<()> {
    let mut h = Harness::paused(|ports| ports.show_error = Some("screen recording denied"))?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    h.input(Input::Show);
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("screen recording denied"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn s_outside_a_hold_records_no_screen() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    h.input(Input::Show);
    h.start().await?;
    h.input(Input::Show);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(h.shows.try_recv().is_err());
    assert!(!h.snapshot().show);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_microphone_failure_ends_the_show_and_frees_the_screen() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Show);
    receive(&h.shows).await?;
    events
        .send(CaptureEvent::Finished(
            id,
            Err(anyhow!("Microphone disconnected")),
        ))
        .await?;
    h.phase(Phase::Error).await?;
    // The screen recorder retired with the session, so the next Show records again.
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_stops_recording_explains_why_and_frees_the_microphone() -> anyhow::Result<()>
{
    let mut h = Harness::paused(|_| {})?;
    let (id, events) = start_show(&mut h).await?;
    events
        .send(ShowEvent::Finished(
            id,
            Err(anyhow!("Screen recording is off")),
        ))
        .await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Screen recording is off"));
    h.input(Input::Release);
    h.input(Input::Press);
    receive(&h.captures).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn footage_from_an_earlier_show_never_reaches_a_later_one() -> anyhow::Result<()> {
    let mut h = Harness::paused(|_| {})?;
    let (earlier, stale) = start_show(&mut h).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    let mut late = footage();
    late.ended = Moment::from_millis(9_999);
    stale.send(ShowEvent::Finished(earlier, Ok(late))).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("this one").await?;
    let recorded = receive(&h.published).await?;
    assert_eq!(recorded.recording.duration, footage().ended);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_queued_behind_a_retiring_screen_still_records_it() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(2);
    let mut h = Harness::paused(|ports| ports.screen_retirement = Some(retirement))?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("first").await?;
    receive(&h.published).await?;
    retiring.reached().await?;
    // The second Show's narration ends before the first screen recorder lets go.
    h.input(Input::Release);
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show);
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("second").await?;
    assert!(still_pending(h.shows.recv()).await);
    retiring.release().await?;
    receive(&h.shows).await?;
    let recorded = receive(&h.published).await?;
    assert_eq!(
        recorded.recording.narration,
        Narration::Text("second".into())
    );
    retiring.reached().await?;
    retiring.release().await?;
    h.close().await
}
