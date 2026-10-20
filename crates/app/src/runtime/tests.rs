//! Drives the production owner through fake ports on the test's paused clock, where deadlines are
//! exact and a pending check proves quiescence. One smoke test and the latency profile host the
//! owner on its own thread instead, as production does, where a pending check only samples.

use std::{path::PathBuf, time::Duration};

use anyhow::{anyhow, bail, ensure};
use speakeasy_platform::{Delivery, InsertPermit, Inserted};
use speakeasy_show::{Image, Journal, Moment, Narration, Size};
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};

use super::{
    ports::{Capturing, Sealed, Speech},
    *,
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

/// Fake devices, speech, insertion, and publishing. Each optional gate or result lane pauses or
/// steers one side effect.
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
    publish_panics: bool,
    captures: Sender<OpenedCapture>,
    shows: Sender<OpenedShow>,
    jobs: Sender<TranscriptionJob>,
    pasted: Sender<String>,
    published: Sender<(Footage, Narration)>,
    loads: Sender<()>,
}

/// What the fakes report back to the test.
struct Reports {
    captures: Receiver<OpenedCapture>,
    shows: Receiver<OpenedShow>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    published: Receiver<(Footage, Narration)>,
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
            publish_panics: false,
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
        let releases = [
            &self.retirement,
            &self.screen_retirement,
            &self.stop,
            &self.publication,
        ]
        .into_iter()
        .flatten()
        .map(|gate| gate.release.clone());
        let insertion = self.insertion.iter().map(|gate| gate.release.clone());
        CleanupGates(releases.chain(insertion).collect())
    }
}

impl Ports for FakePorts {
    type Microphone = FakeMicrophone;
    type Screen = FakeScreen;
    type Speech = FakeSpeech;

    fn prepares_desktop(&self) -> bool {
        self.desktop_pending
    }

    fn record_microphone(
        &self,
        id: SessionId,
        _: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<FakeMicrophone> {
        if let Some(error) = self.record_error {
            bail!(error);
        }
        self.captures.try_send((id, events.clone()))?;
        events.try_send(CaptureEvent::Ready(id))?;
        Ok(FakeMicrophone {
            id,
            events,
            delayed_finish: self.delayed_finish,
            retirement: self.retirement.clone(),
        })
    }

    fn record_screen(
        &self,
        id: SessionId,
        _: Instant,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<FakeScreen> {
        if let Some(error) = self.show_error {
            bail!(error);
        }
        self.shows.try_send((id, events.clone()))?;
        Ok(FakeScreen {
            id,
            events,
            retirement: self.screen_retirement.clone(),
        })
    }

    fn publish(
        &self,
        footage: Footage,
        narration: Narration,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        let published = self.published.clone();
        let publication = self.publication.clone();
        let publish_error = self.publish_error;
        let publish_panics = self.publish_panics;
        async move {
            if let Some(gate) = publication {
                gate.pass(()).await?;
            }
            if let Some(error) = publish_error {
                bail!(error);
            }
            assert!(!publish_panics, "The fake publication panicked");
            if !permit.commit() {
                return Ok(Published::Cancelled);
            }
            published.try_send((footage, narration))?;
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

struct FakeMicrophone {
    id: SessionId,
    events: Sender<CaptureEvent>,
    delayed_finish: bool,
    retirement: Option<Gate<()>>,
}

impl Capturing for FakeMicrophone {
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
struct FakeScreen {
    id: SessionId,
    events: Sender<ShowEvent>,
    retirement: Option<Gate<()>>,
}

impl Capturing for FakeScreen {
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

    async fn transcribe_words(&self, wav: Vec<u8>, language: &str) -> anyhow::Result<Narration> {
        self.transcribe(wav, language).await.map(Narration::Text)
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

/// The owner as a task on the test's executor. Its exit lane closes once the loop returns or unwinds,
/// as `OwnedThread`'s does, so a shutdown check can poll it any number of times; only `close` polls
/// the task itself.
struct OwnedTask {
    /// Never sends; it closes once the owner's loop has finished.
    exit: Sender<Infallible>,
    task: JoinHandle<()>,
}

impl OwnedTask {
    fn spawn(run: impl Future<Output = ()> + Send + 'static) -> Self {
        let (exit, alive) = async_channel::bounded(1);
        let task = tokio::spawn(async move {
            run.await;
            drop(alive);
        });
        Self { exit, task }
    }
}

/// The production controls and owner, plus the test's view of the fakes. Gates close before the
/// owner is stopped and released.
struct Harness<Owner = OwnedTask> {
    _cleanup: CleanupGates,
    runtime: Runtime<Owner>,
    output: watch::Sender<Snapshot>,
    updates: watch::Receiver<Snapshot>,
    levels: watch::Receiver<Meter>,
    captures: Receiver<OpenedCapture>,
    shows: Receiver<OpenedShow>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    published: Receiver<(Footage, Narration)>,
    loads: Receiver<()>,
}

impl Harness {
    fn new() -> anyhow::Result<Self> {
        Self::with_ports(Config::default(), |_| {})
    }

    /// Runs the owner on this test's executor, so `tokio::time::advance` drives its deadlines.
    fn with_ports(config: Config, customize: impl FnOnce(&mut FakePorts)) -> anyhow::Result<Self> {
        Self::host(config, customize, |config, output, meter, ports| {
            let (input, inputs) = input_lane();
            Runtime::host(config, output, meter, ports, input, inputs, |wiring| {
                Ok(OwnedTask::spawn(owner::run(wiring)))
            })
        })
    }

    /// Resolves once the owner has finished its cleanup and exited.
    fn exited(&self) -> impl Future<Output = ()> + use<> {
        let exit = self.runtime.owner.exit.clone();
        async move { exit.closed().await }
    }

    /// Stops the owner and joins it, surfacing its panic; a shutdown stuck on a held gate fails
    /// once the paused clock reaches `PATIENCE`.
    async fn close(mut self) -> anyhow::Result<()> {
        self.runtime.request_stop();
        timeout(PATIENCE, &mut self.runtime.owner.task).await??;
        Ok(())
    }
}

impl Harness<OwnedThread> {
    /// Hosts the owner on its own thread through `Runtime::start_with`, as production does.
    fn on_own_thread(
        config: Config,
        customize: impl FnOnce(&mut FakePorts),
    ) -> anyhow::Result<Self> {
        Self::host(config, customize, Runtime::start_with)
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

/// Whether `future` is still pending once every task has gone idle on the paused clock. On a real
/// thread, it only samples 30 ms.
async fn still_pending(future: impl Future) -> bool {
    timeout(Duration::from_millis(30), future).await.is_err()
}

mod dictation;
mod model;
mod shows;
