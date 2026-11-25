use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, ensure};
use speakeasy_platform::{Delivery, Enter, InsertPermit, Inserted, Scope, Taps};
use speakeasy_show::{Image, Journal, Moment, Narration, NarrationLoss, Recording, Size};
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};

use super::{
    ports::{
        Capturing, Footage, Listening, LiveAudio, Lost, MicrophoneSource, Piece, Pinning,
        Preservation, Sealed, Speech, Unchosen,
    },
    *,
};
use crate::{
    control::Command,
    recognition_terms::Terms,
    testing::{PATIENCE, still_pending},
};

type TranscriptReply = oneshot::Sender<anyhow::Result<String>>;
type TranscriptionJob = (String, TranscriptReply);
type OpenedCapture = (SessionId, Sender<CaptureEvent>);
type OpenedShow = (SessionId, Scope, Sender<ShowEvent>);

#[derive(Debug, PartialEq, Eq)]
enum Sound {
    Played(Cue),
    Expected(SessionId),
    Held(bool),
}

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

struct FakePorts {
    desktop_pending: bool,
    microphone_error: Option<&'static str>,
    delayed_finish: bool,
    load_results: Option<Receiver<anyhow::Result<()>>>,
    recorded_requests: Option<Sender<Vec<u8>>>,
    segmentation: Option<Sender<bool>>,
    retirement: Option<Gate<()>>,
    stop: Option<Gate<()>>,
    insertion: Option<Gate<InsertPermit>>,
    insert_results: Option<Receiver<Inserted>>,
    screen_error: Option<&'static str>,
    screen_report: ScreenReport,
    show_recovery: Option<Gate<InsertPermit>>,
    checkpoint_root: Option<PathBuf>,
    publish_outcome: PublishOutcome,
    saved: Option<Gate<()>>,
    copy_error: Option<&'static str>,
    captures: Sender<OpenedCapture>,
    shows: Sender<OpenedShow>,
    pins: Sender<(SessionId, Taps)>,
    jobs: Sender<TranscriptionJob>,
    pasted: Sender<String>,
    entered: Sender<String>,
    published: Sender<(Footage, Narration)>,
    loads: Sender<()>,
    sounds: Sender<Sound>,
    awake: Sender<Option<Keep>>,
}

struct FakeAwake(Sender<Option<Keep>>);

impl Drop for FakeAwake {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A harness that already closed no longer watches the guard"
        )]
        let _ = self.0.try_send(None);
    }
}

struct Reports {
    captures: Receiver<OpenedCapture>,
    shows: Receiver<OpenedShow>,
    pins: Receiver<(SessionId, Taps)>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    entered: Receiver<String>,
    published: Receiver<(Footage, Narration)>,
    loads: Receiver<()>,
    sounds: Receiver<Sound>,
    awake: Receiver<Option<Keep>>,
}

impl FakePorts {
    fn new() -> (Self, Reports) {
        let (captures, opened) = async_channel::bounded(8);
        let (shows, opened_shows) = async_channel::bounded(8);
        let (pins, pinned) = async_channel::bounded(8);
        let (jobs, requested) = async_channel::bounded(8);
        let (pasted, submitted) = async_channel::bounded(8);
        let (entered, sent) = async_channel::bounded(8);
        let (published, written) = async_channel::bounded(8);
        let (loads, loaded) = async_channel::bounded(8);
        let (sounds, played) = async_channel::unbounded();
        let (awake, kept) = async_channel::unbounded();
        let ports = Self {
            desktop_pending: false,
            microphone_error: None,
            delayed_finish: false,
            load_results: None,
            recorded_requests: None,
            segmentation: None,
            retirement: None,
            stop: None,
            insertion: None,
            insert_results: None,
            screen_error: None,
            screen_report: ScreenReport::Footage,
            show_recovery: None,
            checkpoint_root: None,
            publish_outcome: PublishOutcome::Saves,
            saved: None,
            copy_error: None,
            captures,
            shows,
            pins,
            jobs,
            pasted,
            entered,
            published,
            loads,
            sounds,
            awake,
        };
        let reports = Reports {
            captures: opened,
            shows: opened_shows,
            pins: pinned,
            jobs: requested,
            pasted: submitted,
            entered: sent,
            published: written,
            loads: loaded,
            sounds: played,
            awake: kept,
        };
        (ports, reports)
    }

    fn cleanup_gates(&self) -> CleanupGates {
        let releases = [&self.retirement, &self.stop, &self.saved]
            .into_iter()
            .flatten()
            .map(|gate| gate.release.clone());
        let permits = [&self.insertion, &self.show_recovery]
            .into_iter()
            .flatten()
            .map(|gate| gate.release.clone());
        CleanupGates(releases.chain(permits).collect())
    }
}

impl Ports for FakePorts {
    type Awake = FakeAwake;
    type Microphone = FakeMicrophone;
    type Screen = FakeScreen;
    type Speech = FakeSpeech;

    fn prepares_desktop(&self) -> bool {
        self.desktop_pending
    }

    fn keep_awake(&self, keep: Keep) -> Option<Self::Awake> {
        self.awake
            .try_send(Some(keep))
            .expect("The awake report channel is unbounded");
        Some(FakeAwake(self.awake.clone()))
    }

    fn record_microphone(
        &self,
        id: SessionId,
        _: Option<&str>,
        recognition: RecognitionMode,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<FakeMicrophone> {
        if let Some(error) = self.microphone_error {
            bail!(error);
        }
        if let Some(segmentation) = &self.segmentation {
            segmentation.try_send(recognition == RecognitionMode::Segmented)?;
        }
        self.captures.try_send((id, events.clone()))?;
        events.try_send(CaptureEvent::Ready(id, MicrophoneSource::Chosen))?;
        Ok(FakeMicrophone {
            id,
            events,
            delayed_finish: self.delayed_finish,
            retirement: self.retirement.clone(),
            sounds: self.sounds.clone(),
        })
    }

    fn keep(&self, recorded: SystemTime) -> anyhow::Result<Option<show::Kept>> {
        self.checkpoint_root
            .clone()
            .map(|parent| show::Keeper::start(move || Ok(parent), recorded))
            .transpose()
    }

    fn record_screen(
        &self,
        id: SessionId,
        _: Instant,
        scope: Scope,
        _: Option<show::FrameLog>,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<FakeScreen> {
        if let Some(error) = self.screen_error {
            bail!(error);
        }
        self.shows.try_send((id, scope, events.clone()))?;
        Ok(FakeScreen {
            id,
            events,
            report: self.screen_report,
            pins: self.pins.clone(),
        })
    }

    fn reopen(
        &self,
        interrupted: Interrupted,
    ) -> impl Future<Output = anyhow::Result<show::Reopened>> + Send + 'static {
        show::reopen(interrupted)
    }

    fn publish(
        &self,
        publication: show::Publication,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        let show::Publication { footage, origin } = publication;
        let (checkpoint, recovered) = match origin {
            show::Origin::Live { checkpoint } => (checkpoint, false),
            show::Origin::Recovered { checkpoint, .. } => (Some(checkpoint), true),
        };
        let narration = footage.recording.narration.clone();
        let lost = footage.recording.narration_lost;
        let recovery = self.show_recovery.clone();
        let published = self.published.clone();
        let outcome = self.publish_outcome;
        let saved = self.saved.clone();
        let copy_error = self.copy_error;
        async move {
            if let Some(checkpoint) = checkpoint {
                checkpoint
                    .release(speakeasy_core::machine::Release::Keep)
                    .await?;
            }
            if recovered {
                if let Some(gate) = recovery {
                    gate.pass(permit.clone()).await?;
                }
                return Ok(if permit.commit() {
                    Published::Recovered {
                        show: saved_show(&narration),
                        preservation: Preservation {
                            narration_lost: lost,
                            ..Preservation::default()
                        },
                        narrated_copy: false,
                    }
                } else {
                    Published::Cancelled
                });
            }
            match outcome {
                PublishOutcome::Saves => {},
                PublishOutcome::Fails(error) => bail!(error),
            }
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
                preservation: Preservation {
                    narration_lost: lost,
                    ..Preservation::default()
                },
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
            recorded_requests: self.recorded_requests.clone(),
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
        enter: Enter,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let pasted = match enter {
            Enter::Withheld => self.pasted.clone(),
            Enter::Pressed => self.entered.clone(),
        };
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
    fn whole(&self) {}

    fn keep(&self, _: show::AudioLog, _: Duration, _: &str) -> bool {
        true
    }

    fn expect_cue(&self) {
        self.sounds
            .try_send(Sound::Expected(self.id))
            .expect("The sound lane is unbounded");
    }
}

#[derive(Clone, Copy)]
enum PublishOutcome {
    Saves,
    Fails(&'static str),
}

#[derive(Clone, Copy)]
enum ScreenReport {
    Footage,
    Unchosen(Unchosen),
    Withheld,
}

struct FakeScreen {
    id: SessionId,
    events: Sender<ShowEvent>,
    report: ScreenReport,
    pins: Sender<(SessionId, Taps)>,
}

impl Capturing for FakeScreen {
    fn finish(&self) {
        let report = match self.report {
            ScreenReport::Footage => ShowEvent::Finished(self.id, Ok(footage())),
            ScreenReport::Unchosen(unchosen) => ShowEvent::Unchosen(self.id, unchosen),
            ScreenReport::Withheld => return,
        };
        self.events
            .try_send(report)
            .expect("Show lane has room for the fake's report");
    }

    fn retire(self) -> impl Future<Output = ()> + Send + 'static {
        std::future::ready(())
    }
}

impl Pinning for FakeScreen {
    fn pin(&self, taps: Taps) {
        self.pins
            .try_send((self.id, taps))
            .expect("Pin lane has room for the test's pins");
    }
}

const SPOKEN: Duration = Duration::from_secs(2);

fn sealed() -> Sealed {
    Sealed {
        wav: vec![1, 2],
        trimmed: Duration::ZERO,
        audible: SPOKEN,
        partition: None,
    }
}

fn piece() -> Piece {
    let Sealed { wav, audible, .. } = sealed();
    Piece { wav, audible }
}

fn disconnected(id: SessionId) -> CaptureEvent {
    CaptureEvent::Lost(
        id,
        sealed(),
        Lost {
            notice: "The microphone was disconnected.",
            failure: anyhow!("The microphone was disconnected. Reconnect it."),
        },
    )
}

fn footage() -> Box<Footage> {
    let frame = Image::from_bgra(2, 2, vec![128; 16]).expect("A 2×2 frame");
    let mut journal = Journal::default();
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
        framing: speakeasy_show::Framing::Automatic,
    })
}

fn saved_show(narration: &Narration) -> SavedShow {
    SavedShow::assume(match narration {
        Narration::Text(text) => format!("shows/{text}"),
        Narration::Words(_) => "shows/silent".to_owned(),
    })
}

struct FakeSpeech {
    jobs: Sender<TranscriptionJob>,
    stop: Option<Gate<()>>,
    recorded_requests: Option<Sender<Vec<u8>>>,
}

impl Speech for FakeSpeech {
    async fn transcribe_stream(
        &mut self,
        mut audio: LiveAudio,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<Option<String>> {
        while audio.frames.recv().await.is_some() {}
        if audio.tail.await.is_err() {
            return Ok(None);
        }
        self.transcribe(Vec::new(), language, terms).await.map(Some)
    }

    async fn transcribe(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        _terms: &Terms,
    ) -> anyhow::Result<String> {
        if let Some(requests) = &self.recorded_requests {
            requests.try_send(wav)?;
        }
        let (reply, result) = oneshot::channel();
        self.jobs.send((language.to_owned(), reply)).await?;
        result.await?
    }

    async fn transcribe_words(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<Narration> {
        self.transcribe(wav, language, terms)
            .await
            .map(Narration::Text)
    }

    fn probe_with_silence(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    async fn stop(&mut self) {
        if let Some(gate) = &self.stop {
            gate.pass_during_cleanup(()).await;
        }
    }
}

struct CleanupGates(Vec<Receiver<()>>);

impl Drop for CleanupGates {
    fn drop(&mut self) {
        for gate in &self.0 {
            gate.close();
        }
    }
}

struct OwnedTask {
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
    fn on_task<P: Ports>(
        config: Config,
        styles: watch::Receiver<Style>,
        snapshots: watch::Sender<Snapshot>,
        meter: watch::Sender<f32>,
        ports: P,
    ) -> anyhow::Result<Self> {
        let configuration = Configuration {
            effective: config.clone(),
            saved: config,
            styles,
        };
        Self::on_task_with(configuration, snapshots, meter, ports)
    }

    fn on_task_with<P: Ports>(
        configuration: Configuration,
        snapshots: watch::Sender<Snapshot>,
        meter: watch::Sender<f32>,
        ports: P,
    ) -> anyhow::Result<Self> {
        let (input, inputs) = input_lane();
        Self::host(
            configuration,
            snapshots,
            meter,
            ports,
            input,
            inputs,
            |wiring| Ok(OwnedTask::spawn(owner::run(wiring))),
        )
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        self.request_stop();
        timeout(PATIENCE, &mut self.host.task).await??;
        Ok(())
    }
}

struct Harness<Host = OwnedTask> {
    _cleanup: CleanupGates,
    runtime: Runtime<Host>,
    styles: watch::Sender<Style>,
    updates: watch::Receiver<Snapshot>,
    levels: watch::Receiver<f32>,
    captures: Receiver<OpenedCapture>,
    shows: Receiver<OpenedShow>,
    pins: Receiver<(SessionId, Taps)>,
    jobs: Receiver<TranscriptionJob>,
    pasted: Receiver<String>,
    entered: Receiver<String>,
    published: Receiver<(Footage, Narration)>,
    loads: Receiver<()>,
    sounds: Receiver<Sound>,
    awake: Receiver<Option<Keep>>,
}

impl Harness {
    fn new() -> anyhow::Result<Self> {
        Self::with_ports(Config::default(), |_| {})
    }

    fn with_ports(config: Config, customize: impl FnOnce(&mut FakePorts)) -> anyhow::Result<Self> {
        Self::host(config, None, customize, Runtime::on_task)
    }

    fn remembering(
        show: SavedShow,
        customize: impl FnOnce(&mut FakePorts),
    ) -> anyhow::Result<Self> {
        Self::host(Config::default(), Some(show), customize, Runtime::on_task)
    }

    fn resuming(effective: Config, saved: Config) -> anyhow::Result<Self> {
        Self::host(
            effective,
            None,
            |_| {},
            |effective, styles, snapshots, meter, ports| {
                let configuration = Configuration {
                    effective,
                    saved,
                    styles,
                };
                Runtime::on_task_with(configuration, snapshots, meter, ports)
            },
        )
    }

    fn exited(&self) -> impl Future<Output = ()> + use<> {
        let exit = self.runtime.host.exit.clone();
        async move { exit.closed().await }
    }

    async fn close(mut self) -> anyhow::Result<()> {
        self.runtime.close().await
    }
}

impl Harness<OwnedThread> {
    fn on_own_thread(
        config: Config,
        customize: impl FnOnce(&mut FakePorts),
    ) -> anyhow::Result<Self> {
        Self::host(config, None, customize, Runtime::start_with)
    }
}

impl<Host: Sync> Harness<Host> {
    fn host(
        config: Config,
        last_show: Option<SavedShow>,
        customize: impl FnOnce(&mut FakePorts),
        start: impl FnOnce(
            Config,
            watch::Receiver<Style>,
            watch::Sender<Snapshot>,
            watch::Sender<f32>,
            FakePorts,
        ) -> anyhow::Result<Runtime<Host>>,
    ) -> anyhow::Result<Self> {
        let (mut ports, reports) = FakePorts::new();
        customize(&mut ports);
        let cleanup = ports.cleanup_gates();

        let (output, updates) = watch::channel(Snapshot {
            last_show,
            ..Snapshot::default()
        });
        let (meter, levels) = watch::channel(0.0_f32);
        let (styles, choices) = watch::channel(Style::default());
        let runtime = start(config, choices, output, meter, ports)?;
        Ok(Self {
            _cleanup: cleanup,
            runtime,
            styles,
            updates,
            levels,
            captures: reports.captures,
            shows: reports.shows,
            pins: reports.pins,
            jobs: reports.jobs,
            pasted: reports.pasted,
            entered: reports.entered,
            published: reports.published,
            loads: reports.loads,
            sounds: reports.sounds,
            awake: reports.awake,
        })
    }

    fn input(&self, input: Input) {
        self.runtime.input.deliver(input);
    }

    async fn call(&self, command: Command) -> anyhow::Result<Reply> {
        let (call, receipt) = Call::new(command);
        self.runtime.control(call);
        receive(&receipt).await
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

    async fn observe_level(&mut self, level: f32) -> anyhow::Result<()> {
        timeout(
            PATIENCE,
            self.levels
                .wait_for(|measured| measured.to_bits() == level.to_bits()),
        )
        .await??;
        Ok(())
    }

    async fn phase(&mut self, wanted: Phase) -> anyhow::Result<()> {
        self.observe(|snapshot| snapshot.phase == wanted).await
    }

    async fn ready(&mut self) -> anyhow::Result<()> {
        receive(&self.loads).await?;
        self.observe(|snapshot| snapshot.model == ModelState::Ready)
            .await
    }

    fn snapshot(&self) -> Snapshot {
        self.updates.borrow().clone()
    }

    fn take_sounds(&self) -> Vec<Sound> {
        std::iter::from_fn(|| self.sounds.try_recv().ok()).collect()
    }

    async fn start(&mut self) -> anyhow::Result<OpenedCapture> {
        self.input(Input::Toggle);
        self.phase(Phase::Recording).await?;
        receive(&self.captures).await
    }

    async fn finish(&self) -> anyhow::Result<TranscriptReply> {
        self.input(Input::Toggle);
        let (language, reply) = receive(&self.jobs).await?;
        assert_eq!(language, self.runtime.baseline.language);
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

fn english() -> Config {
    Config {
        language: "en".into(),
        ..Config::default()
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

async fn arm_practice<Host: Sync>(
    h: &Harness<Host>,
) -> anyhow::Result<mpsc::Receiver<PracticeEvent>> {
    let mut receipt = h.runtime.practice().map_err(anyhow::Error::msg)?;
    assert!(matches!(
        timeout(PATIENCE, receipt.recv()).await?,
        Some(PracticeEvent::Ready)
    ));
    Ok(receipt)
}

async fn practice_text(receipt: &mut mpsc::Receiver<PracticeEvent>) -> anyhow::Result<String> {
    let Some(PracticeEvent::Text(text)) = timeout(PATIENCE, receipt.recv()).await? else {
        bail!("Practice did not receive its transcript");
    };
    Ok(text)
}

fn answer(reply: TranscriptReply, text: &str) -> anyhow::Result<()> {
    ensure!(
        reply.send(Ok(text.to_owned())).is_ok(),
        "The transcription request was abandoned"
    );
    Ok(())
}

async fn start_show(h: &mut Harness) -> anyhow::Result<OpenedShow> {
    start_show_listening(h).await?;
    receive(&h.shows).await
}

async fn start_show_listening(h: &mut Harness) -> anyhow::Result<OpenedCapture> {
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let microphone = receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    Ok(microphone)
}

fn finish_show(h: &Harness) {
    for input in [Input::Release, Input::Press, Input::Release] {
        h.input(input);
    }
}

fn change_settings(h: &mut Harness) {
    let mut config = h.runtime.baseline.clone();
    config.remove_fillers = !config.remove_fillers;
    h.runtime.configure(ValidConfig::assume(config));
}

mod control;
mod cues;
mod dictation;
mod last_show;
mod practice;
mod printing;
mod recognition;
mod recovery;
mod replay;
mod shows;
mod streaming;
mod styles;
