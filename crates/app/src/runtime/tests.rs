//! Drives the production owner through fake ports on the test's paused clock, where deadlines are
//! exact and a pending check proves quiescence. One smoke test and the latency profile host the
//! owner on its own thread instead, as production does, where a pending check only samples.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure};
use speakeasy_platform::{Delivery, InsertPermit, Inserted, Scope, Taps};
use speakeasy_show::{Image, Journal, Moment, Narration, NarrationLoss, Recording, Size};
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};

use super::{
    ports::{Capturing, Listening, Pinning, Sealed, Speech, Unchosen},
    *,
};
use crate::testing::{PATIENCE, still_pending};

type Reply = oneshot::Sender<anyhow::Result<String>>;
type TranscriptionJob = (String, Reply);
type OpenedCapture = (SessionId, Sender<CaptureEvent>);
type OpenedShow = (SessionId, Scope, Sender<ShowEvent>);

/// A cue the owner played, a microphone it told to expect one, or the cue output held open or
/// released, in the order they happened.
#[derive(Debug, PartialEq, Eq)]
enum Sound {
    Played(Cue),
    Expected(SessionId),
    Held(bool),
}

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
    microphone_error: Option<&'static str>,
    delayed_finish: bool,
    load_results: Option<Receiver<anyhow::Result<()>>>,
    recovery: Option<Receiver<anyhow::Result<()>>>,
    retirement: Option<Gate<()>>,
    stop: Option<Gate<()>>,
    insertion: Option<Gate<InsertPermit>>,
    /// Each insertion takes the next queued result once it commits, or reports `Sent`.
    insert_results: Option<Receiver<Inserted>>,
    screen_error: Option<&'static str>,
    /// What the screen reports in place of footage when finished, as an area Show finished before
    /// its area was chosen does.
    screen_unchosen: Option<Unchosen>,
    screen_retirement: Option<Gate<()>>,
    publication: Option<Gate<()>>,
    publish_error: Option<&'static str>,
    publish_panics: bool,
    /// Holds a publication once its Show is saved, before it reports.
    saved: Option<Gate<()>>,
    copy_error: Option<&'static str>,
    captures: Sender<OpenedCapture>,
    shows: Sender<OpenedShow>,
    pins: Sender<(SessionId, Taps)>,
    jobs: Sender<TranscriptionJob>,
    pasted: Sender<String>,
    published: Sender<(Footage, Narration)>,
    loads: Sender<()>,
    sounds: Sender<Sound>,
}

/// What the fakes report back to the test.
struct Reports {
    captures: Receiver<OpenedCapture>,
    shows: Receiver<OpenedShow>,
    pins: Receiver<(SessionId, Taps)>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    published: Receiver<(Footage, Narration)>,
    loads: Receiver<()>,
    sounds: Receiver<Sound>,
}

impl FakePorts {
    fn new() -> (Self, Reports) {
        let (captures, opened) = async_channel::bounded(8);
        let (shows, opened_shows) = async_channel::bounded(8);
        let (pins, pinned) = async_channel::bounded(8);
        let (jobs, requested) = async_channel::bounded(8);
        let (pasted, submitted) = async_channel::bounded(8);
        let (published, written) = async_channel::bounded(8);
        let (loads, loaded) = async_channel::bounded(8);
        let (sounds, played) = async_channel::unbounded();
        let ports = Self {
            desktop_pending: false,
            microphone_error: None,
            delayed_finish: false,
            load_results: None,
            recovery: None,
            retirement: None,
            stop: None,
            insertion: None,
            insert_results: None,
            screen_error: None,
            screen_unchosen: None,
            screen_retirement: None,
            publication: None,
            publish_error: None,
            publish_panics: false,
            saved: None,
            copy_error: None,
            captures,
            shows,
            pins,
            jobs,
            pasted,
            published,
            loads,
            sounds,
        };
        let reports = Reports {
            captures: opened,
            shows: opened_shows,
            pins: pinned,
            jobs: requested,
            pasted: submitted,
            published: written,
            loads: loaded,
            sounds: played,
        };
        (ports, reports)
    }

    fn cleanup_gates(&self) -> CleanupGates {
        let releases = [
            &self.retirement,
            &self.screen_retirement,
            &self.stop,
            &self.publication,
            &self.saved,
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
        if let Some(error) = self.microphone_error {
            bail!(error);
        }
        self.captures.try_send((id, events.clone()))?;
        events.try_send(CaptureEvent::Ready(id))?;
        Ok(FakeMicrophone {
            id,
            events,
            delayed_finish: self.delayed_finish,
            retirement: self.retirement.clone(),
            sounds: self.sounds.clone(),
        })
    }

    fn record_screen(
        &self,
        id: SessionId,
        _: Instant,
        scope: Scope,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<FakeScreen> {
        if let Some(error) = self.screen_error {
            bail!(error);
        }
        self.shows.try_send((id, scope, events.clone()))?;
        Ok(FakeScreen {
            id,
            events,
            unchosen: self.screen_unchosen,
            pins: self.pins.clone(),
            retirement: self.screen_retirement.clone(),
        })
    }

    fn publish(
        &self,
        footage: Footage,
        narration: Narration,
        lost: Option<NarrationLoss>,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        let published = self.published.clone();
        let publication = self.publication.clone();
        let publish_error = self.publish_error;
        let publish_panics = self.publish_panics;
        let saved = self.saved.clone();
        let copy_error = self.copy_error;
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
            let show = saved_show(&narration);
            let ending = footage.recording.ending;
            published.try_send((footage, narration))?;
            if let Some(gate) = saved {
                gate.pass(()).await?;
            }
            Ok(Published::Saved {
                show,
                ending,
                lost,
                copied: copy_error.map_or(Ok(()), |error| Err(anyhow!(error))),
            })
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

    fn cue(&self, cue: Cue) {
        self.sounds
            .try_send(Sound::Played(cue))
            .expect("The sound lane is unbounded");
    }

    fn hold_cues(&self, held: bool) {
        self.sounds
            .try_send(Sound::Held(held))
            .expect("The sound lane is unbounded");
    }
}

struct FakeMicrophone {
    id: SessionId,
    events: Sender<CaptureEvent>,
    delayed_finish: bool,
    retirement: Option<Gate<()>>,
    sounds: Sender<Sound>,
}

impl Capturing for FakeMicrophone {
    fn finish(&self) {
        if self.delayed_finish {
            return;
        }
        self.events
            .try_send(CaptureEvent::Finished(self.id, Ok(Some(sealed()))))
            .expect("Capture lane has room for fake audio");
    }

    async fn retire(self) {
        if let Some(gate) = self.retirement {
            gate.pass_during_cleanup(()).await;
        }
    }
}

impl Listening for FakeMicrophone {
    fn expect_cue(&self) {
        self.sounds
            .try_send(Sound::Expected(self.id))
            .expect("The sound lane is unbounded");
    }
}

/// Reports one gray frame as its footage when finished, or that nothing was chosen when
/// `unchosen` says so, and each pin it takes.
struct FakeScreen {
    id: SessionId,
    events: Sender<ShowEvent>,
    unchosen: Option<Unchosen>,
    pins: Sender<(SessionId, Taps)>,
    retirement: Option<Gate<()>>,
}

impl Capturing for FakeScreen {
    fn finish(&self) {
        let report = match self.unchosen {
            Some(unchosen) => ShowEvent::Unchosen(self.id, unchosen),
            None => ShowEvent::Finished(self.id, Ok(footage())),
        };
        self.events
            .try_send(report)
            .expect("Show lane has room for the fake's report");
    }

    async fn retire(self) {
        if let Some(gate) = self.retirement {
            gate.pass_during_cleanup(()).await;
        }
    }
}

impl Pinning for FakeScreen {
    fn pin(&self, taps: Taps) {
        self.pins
            .try_send((self.id, taps))
            .expect("Pin lane has room for the test's pins");
    }
}

fn sealed() -> Sealed {
    Sealed {
        wav: vec![1, 2],
        trimmed: Duration::ZERO,
    }
}

fn footage() -> Box<Footage> {
    let frame = Image::from_bgra(2, 2, vec![128; 16]).expect("A 2×2 frame");
    let mut journal = Journal::new();
    journal
        .admit()
        .expect("An empty journal admits a frame")
        .record(Moment::START, frame);
    Box::new(Footage {
        recording: Recording {
            duration: Moment::from_millis(1_000),
            surface: Size {
                width: 1000.0,
                height: 1000.0,
            },
            ..Recording::default()
        },
        journal,
    })
}

/// The folder a fake publication saves a Show in, named for what it said so a test can tell Shows
/// apart; nothing is written there.
fn saved_show(narration: &Narration) -> SavedShow {
    SavedShow::assume(match narration {
        Narration::Text(text) => format!("shows/{text}"),
        Narration::Words(_) => "shows/silent".to_owned(),
    })
}

struct FakeSpeech {
    jobs: Sender<TranscriptionJob>,
    stop: Option<Gate<()>>,
    recovery: Option<Receiver<anyhow::Result<()>>>,
}

impl Speech for FakeSpeech {
    async fn transcribe(&mut self, _: Vec<u8>, language: &str) -> anyhow::Result<String> {
        let (reply, result) = oneshot::channel();
        self.jobs.send((language.to_owned(), reply)).await?;
        result.await?
    }

    async fn transcribe_words(
        &mut self,
        wav: Vec<u8>,
        language: &str,
    ) -> anyhow::Result<Narration> {
        self.transcribe(wav, language).await.map(Narration::Text)
    }

    async fn probe_with_silence(&mut self) -> anyhow::Result<()> {
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

impl Runtime<OwnedTask> {
    /// Hosts the owner on this test's executor, so `tokio::time::advance` drives its deadlines.
    fn on_task<P: Ports>(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
        meter: watch::Sender<Meter>,
        ports: P,
    ) -> anyhow::Result<Self> {
        let (input, inputs) = input_lane();
        Self::host(config, snapshots, meter, ports, input, inputs, |wiring| {
            Ok(OwnedTask::spawn(owner::run(wiring)))
        })
    }

    /// Stops the owner and joins it, surfacing its panic; a shutdown still running when the paused
    /// clock reaches `PATIENCE` fails.
    async fn close(&mut self) -> anyhow::Result<()> {
        self.request_stop();
        timeout(PATIENCE, &mut self.host.task).await??;
        Ok(())
    }
}

/// The production controls and owner, plus the test's view of the fakes. Gates close before the
/// owner is stopped and released.
struct Harness<Host = OwnedTask> {
    _cleanup: CleanupGates,
    runtime: Runtime<Host>,
    output: watch::Sender<Snapshot>,
    updates: watch::Receiver<Snapshot>,
    levels: watch::Receiver<Meter>,
    captures: Receiver<OpenedCapture>,
    shows: Receiver<OpenedShow>,
    pins: Receiver<(SessionId, Taps)>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    published: Receiver<(Footage, Narration)>,
    loads: Receiver<()>,
    sounds: Receiver<Sound>,
}

impl Harness {
    fn new() -> anyhow::Result<Self> {
        Self::with_ports(Config::default(), |_| {})
    }

    fn with_ports(config: Config, customize: impl FnOnce(&mut FakePorts)) -> anyhow::Result<Self> {
        Self::host(config, None, customize, Runtime::on_task)
    }

    /// Starts the owner as one starts after a pause, from a snapshot that presents `show` as the
    /// last Show.
    fn remembering(
        show: SavedShow,
        customize: impl FnOnce(&mut FakePorts),
    ) -> anyhow::Result<Self> {
        Self::host(Config::default(), Some(show), customize, Runtime::on_task)
    }

    /// Resolves once the owner has finished its cleanup and exited.
    fn exited(&self) -> impl Future<Output = ()> + use<> {
        let exit = self.runtime.host.exit.clone();
        async move { exit.closed().await }
    }

    /// Stops the owner and joins it before the fakes' gates close, so a shutdown stuck on a held
    /// gate fails at `PATIENCE`.
    async fn close(mut self) -> anyhow::Result<()> {
        self.runtime.close().await
    }
}

impl Harness<OwnedThread> {
    /// Hosts the owner on its own thread through `Runtime::start_with`, as production does.
    fn on_own_thread(
        config: Config,
        customize: impl FnOnce(&mut FakePorts),
    ) -> anyhow::Result<Self> {
        Self::host(config, None, customize, Runtime::start_with)
    }
}

// Sync keeps the futures of the `&self` helpers Send (clippy::future_not_send).
impl<Host: Sync> Harness<Host> {
    fn host(
        config: Config,
        last_show: Option<SavedShow>,
        customize: impl FnOnce(&mut FakePorts),
        start: impl FnOnce(
            Config,
            watch::Sender<Snapshot>,
            watch::Sender<Meter>,
            FakePorts,
        ) -> anyhow::Result<Runtime<Host>>,
    ) -> anyhow::Result<Self> {
        let (mut ports, reports) = FakePorts::new();
        customize(&mut ports);
        let cleanup = ports.cleanup_gates();
        // A restarted shell's epoch, so a level tagged with the default epoch fails `observe_level`.
        let (output, updates) = watch::channel(Snapshot {
            epoch: SnapshotEpoch::default().next(),
            last_show,
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
            pins: reports.pins,
            jobs: reports.jobs,
            pasted: reports.pasted,
            published: reports.published,
            loads: reports.loads,
            sounds: reports.sounds,
        })
    }

    fn input(&self, input: Input) {
        self.runtime.input.deliver(input);
    }

    fn report(&self, report: Report) -> anyhow::Result<()> {
        ensure!(
            self.runtime.report(report).is_ok(),
            "The owner stopped before the report"
        );
        Ok(())
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
            (snapshot.epoch, snapshot.session),
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

    /// Every sound the fakes reported since the last call, in order.
    fn take_sounds(&self) -> Vec<Sound> {
        std::iter::from_fn(|| self.sounds.try_recv().ok()).collect()
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

/// Two taps of Shift that began now.
fn double_tap() -> Taps {
    let pressed = Instant::now();
    Taps {
        pressed,
        released: pressed
            .checked_add(Duration::from_millis(300))
            .expect("The clock reaches 300 ms on"),
    }
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

/// Holds the shortcut and presses S, returning the screen recorder's lane.
async fn start_show(h: &mut Harness) -> anyhow::Result<OpenedShow> {
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    receive(&h.shows).await
}

/// Lets go of the shortcut that started the Show, then presses and releases it to finish.
fn finish_show(h: &Harness) {
    for input in [Input::Release, Input::Press, Input::Release] {
        h.input(input);
    }
}

/// Applies settings that end any session under way, even a Show being saved.
fn change_settings(h: &Harness) {
    let config = Config {
        remove_fillers: !h.runtime.configuration.borrow().remove_fillers,
        ..h.runtime.configuration.borrow().clone()
    };
    h.runtime.configure(ValidConfig::assume(config));
}

mod cues;
mod dictation;
mod last_show;
mod model;
mod shows;
