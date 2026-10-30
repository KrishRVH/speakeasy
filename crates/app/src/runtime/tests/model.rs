//! Generated sequences of input bursts, clock jumps, audio, footage, transcripts, and settings
//! changes drive the production owner through gate-free fakes whose devices and workers take
//! generated time to release, and whose workers fail to load or answer a probe when generated
//! faults say so. After every step the owner still runs, never reopens a device before its
//! release, never loads a worker before the previous one has stopped or drops one unstopped,
//! inserts a session's text only while that session is the latest, and has retired every device
//! once no session is active; a final drain must bring every session to rest with every device
//! released, and shutdown must stop the last worker. Gated windows stay with the scripted
//! scenarios.

use std::collections::{HashMap, VecDeque};

use proptest::prelude::*;

use super::*;

/// One clock jump past the five-minute limit, so every capturing gesture finishes.
const PAST_THE_LIMIT: Duration = Duration::from_secs(301);
/// Drain rounds before the owner counts as stuck; each round answers everything outstanding.
const DRAIN_ROUNDS: usize = 16;

/// Which of the owner's two recording devices an entry concerns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Source {
    Microphone,
    Screen,
}

/// What the model ports did, in order, on one unbounded lane.
enum Log {
    MicrophoneOpened(SessionId, Sender<CaptureEvent>),
    ScreenOpened(SessionId, Sender<ShowEvent>),
    Finished(Source, SessionId),
    /// The owner asked for the device's release, which takes its generated time.
    Retiring(Source, SessionId),
    Retired(Source, SessionId),
    Pinned(SessionId),
    /// Text was inserted under a permit that still held authority, while `latest` was the latest
    /// session.
    Inserted {
        text: String,
        latest: u64,
    },
    /// A speech worker started.
    Loaded,
    /// A speech worker finished stopping.
    Stopped,
    /// A speech worker was dropped without being stopped.
    Abandoned,
}

/// Ports with no gates: devices log and report nothing on their own beyond the microphone's
/// `Ready`, a worker logs its start and stop and is ready at once unless the next fault fails it,
/// and transcription waits for the test's answer.
struct ModelPorts {
    log: Sender<Log>,
    jobs: Sender<Job>,
    /// Whether each load or probe in turn fails; the rest succeed.
    faults: Receiver<bool>,
    /// What the owner presents, which names the latest session.
    presented: watch::Receiver<Snapshot>,
    /// How long each device or worker takes to release once retired or stopped, on the paused
    /// clock.
    releases: Releases,
}

/// How long the microphone, the screen recorder, and a speech worker take to release.
#[derive(Clone, Copy, Debug)]
struct Releases {
    microphone: Duration,
    screen: Duration,
    worker: Duration,
}

impl ModelPorts {
    fn device(&self, source: Source, id: SessionId) -> ModelDevice {
        let release = match source {
            Source::Microphone => self.releases.microphone,
            Source::Screen => self.releases.screen,
        };
        ModelDevice {
            source,
            id,
            log: self.log.clone(),
            release,
        }
    }
}

impl Ports for ModelPorts {
    type Microphone = ModelDevice;
    type Screen = ModelDevice;
    type Speech = ModelSpeech;

    fn prepares_desktop(&self) -> bool {
        false
    }

    fn record_microphone(
        &self,
        id: SessionId,
        _: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<ModelDevice> {
        self.log
            .try_send(Log::MicrophoneOpened(id, events.clone()))?;
        events.try_send(CaptureEvent::Ready(id))?;
        Ok(self.device(Source::Microphone, id))
    }

    fn record_screen(
        &self,
        id: SessionId,
        _: Instant,
        _: Scope,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<ModelDevice> {
        self.log.try_send(Log::ScreenOpened(id, events))?;
        Ok(self.device(Source::Screen, id))
    }

    fn publish(
        &self,
        footage: Footage,
        _: Narration,
        lost: Option<NarrationLoss>,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        let ending = footage.recording.ending;
        // Commits once the task runs, as publishing does, so a cancel can still come first.
        async move {
            Ok(if permit.commit() {
                Published::Copied { ending, lost }
            } else {
                Published::Cancelled
            })
        }
    }

    fn load(
        &self,
        _: Config,
        _: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<ModelSpeech>> + Send + 'static {
        let (jobs, log, faults) = (self.jobs.clone(), self.log.clone(), self.faults.clone());
        let release = self.releases.worker;
        // Starts once polled, as the port promises.
        async move {
            ensure!(faults.try_recv() != Ok(true), "Model engine failed to load");
            log.try_send(Log::Loaded)?;
            Ok(ModelSpeech {
                jobs,
                log,
                faults,
                release,
                stopped: false,
            })
        }
    }

    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        _: Delivery,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let (log, presented) = (self.log.clone(), self.presented.clone());
        async move {
            if !permit.commit() {
                return Ok(Inserted::Cancelled);
            }
            let latest = presented.borrow().session;
            log.try_send(Log::Inserted { text, latest })?;
            Ok(Inserted::Sent)
        }
    }

    fn cue(&self, _: Cue) {}

    fn hold_cues(&self, _: bool) {}
}

struct ModelDevice {
    source: Source,
    id: SessionId,
    log: Sender<Log>,
    release: Duration,
}

impl ModelDevice {
    fn log(&self, entry: Log) {
        self.log
            .try_send(entry)
            .expect("The model log stays open for the whole case");
    }
}

impl Capturing for ModelDevice {
    fn finish(&self) {
        self.log(Log::Finished(self.source, self.id));
    }

    fn retire(self) -> impl Future<Output = ()> + Send + 'static {
        self.log(Log::Retiring(self.source, self.id));
        async move {
            tokio::time::sleep(self.release).await;
            self.log(Log::Retired(self.source, self.id));
        }
    }
}

impl Listening for ModelDevice {
    fn expect_cue(&self) {}
}

impl Pinning for ModelDevice {
    fn pin(&self, _: Taps) {
        self.log(Log::Pinned(self.id));
    }
}

struct ModelSpeech {
    jobs: Sender<Job>,
    log: Sender<Log>,
    faults: Receiver<bool>,
    /// How long stopping takes; a successor that loads meanwhile overlaps this worker.
    release: Duration,
    stopped: bool,
}

impl Drop for ModelSpeech {
    fn drop(&mut self) {
        if !self.stopped {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A case that already failed may have closed the log; a destructor must not panic"
            )]
            let _ = self.log.try_send(Log::Abandoned);
        }
    }
}

impl Speech for ModelSpeech {
    async fn transcribe(&mut self, wav: Vec<u8>, _: &str) -> anyhow::Result<String> {
        let (reply, result) = oneshot::channel();
        self.jobs.try_send((wav, reply))?;
        result.await?
    }

    async fn transcribe_words(
        &mut self,
        wav: Vec<u8>,
        language: &str,
    ) -> anyhow::Result<Narration> {
        self.transcribe(wav, language).await.map(Narration::Text)
    }

    fn probe_with_silence(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send {
        let responds = self.faults.try_recv() != Ok(true);
        std::future::ready(if responds {
            Ok(())
        } else {
            Err(anyhow!("Model engine stopped responding"))
        })
    }

    async fn stop(&mut self) {
        tokio::time::sleep(self.release).await;
        self.stopped = true;
        self.log
            .try_send(Log::Stopped)
            .expect("The model log stays open for the whole case");
    }
}

#[derive(Clone, Copy, Debug)]
enum Key {
    Press,
    Release,
    Lock,
    Toggle,
    Escape,
    Cancel,
    Interrupt,
    Show,
    Pin,
}

impl Key {
    fn input(self) -> Input {
        match self {
            Self::Press => Input::Press,
            Self::Release => Input::Release,
            Self::Lock => Input::Lock,
            Self::Toggle => Input::Toggle,
            Self::Escape => Input::Escape,
            Self::Cancel => Input::Cancel,
            Self::Interrupt => Input::Interrupt,
            Self::Show => Input::Show(Scope::Screen),
            Self::Pin => Input::Pin(double_tap()),
        }
    }
}

/// A Show starts with S during a hold; the shortcut is then let go while the Show records.
const SHOW_GESTURE: [Key; 3] = [Key::Press, Key::Show, Key::Release];

/// How the latest microphone's capture ends.
#[derive(Clone, Copy, Debug)]
enum Heard {
    Speech,
    Silence,
    Failure,
}

/// How the oldest outstanding transcription is answered.
#[derive(Clone, Copy, Debug)]
enum Said {
    Words,
    Nothing,
    Failure,
}

/// How the latest screen's recording ends.
#[derive(Clone, Copy, Debug)]
enum Seen {
    Complete,
    Failure,
    /// Its scope found nothing to record, as a W over no window or an A without an area does.
    Unchosen,
}

/// Which setting a change flips.
#[derive(Clone, Copy, Debug)]
enum Setting {
    /// Read per session, so a session in progress ends and a cancelled transcription's worker is
    /// probed or replaced.
    Fillers,
    /// Read by the engine, so the worker reloads; flipping it also switches recovery between
    /// probing and replacing.
    Gpu,
}

#[derive(Clone, Debug)]
enum Step {
    /// Delivered back to back, so the owner takes them all before any release that lands meanwhile.
    Inputs(Vec<Key>),
    Advance(Duration),
    /// The latest microphone's capture ends, whether or not it was asked to finish.
    Audio(Heard),
    /// The latest screen's recording ends: complete once asked to finish, failed or with nothing
    /// to record at any time.
    Footage(Seen),
    Answer(Said),
    Configure(Setting),
}

fn step() -> impl Strategy<Value = Step> {
    let key = prop_oneof![
        3 => Just(Key::Press),
        2 => Just(Key::Release),
        1 => Just(Key::Lock),
        1 => Just(Key::Toggle),
        1 => Just(Key::Escape),
        2 => Just(Key::Cancel),
        1 => Just(Key::Interrupt),
        1 => Just(Key::Show),
        1 => Just(Key::Pin),
    ];
    let inputs = prop_oneof![
        1 => prop::collection::vec(key, 1..=3),
        1 => Just(SHOW_GESTURE.to_vec()),
    ];
    let advance = prop_oneof![
        9 => (0_u64..400).prop_map(Duration::from_millis),
        1 => Just(PAST_THE_LIMIT),
    ];
    let heard = prop_oneof![
        Just(Heard::Speech),
        Just(Heard::Silence),
        Just(Heard::Failure)
    ];
    let said = prop_oneof![Just(Said::Words), Just(Said::Nothing), Just(Said::Failure)];
    let seen = prop_oneof![
        Just(Seen::Complete),
        Just(Seen::Failure),
        Just(Seen::Unchosen)
    ];
    let setting = prop_oneof![Just(Setting::Fillers), Just(Setting::Gpu)];
    prop_oneof![
        4 => inputs.prop_map(Step::Inputs),
        2 => advance.prop_map(Step::Advance),
        2 => heard.prop_map(Step::Audio),
        2 => seen.prop_map(Step::Footage),
        2 => said.prop_map(Step::Answer),
        1 => setting.prop_map(Step::Configure),
    ]
}

fn releases() -> impl Strategy<Value = Releases> {
    let release = prop_oneof![
        Just(Duration::ZERO),
        (1_u64..400).prop_map(Duration::from_millis),
    ];
    (release.clone(), release.clone(), release).prop_map(|(microphone, screen, worker)| Releases {
        microphone,
        screen,
        worker,
    })
}

/// A transcription request: the WAV, which names the session that spoke it, and its answer.
type Job = (Vec<u8>, Reply);

/// Speech whose WAV names the session that spoke it, so its transcript can.
fn spoken_by(id: SessionId) -> Sealed {
    Sealed {
        wav: u64::from(id).to_le_bytes().to_vec(),
        trimmed: Duration::ZERO,
    }
}

/// The transcript of speech `spoken_by` a session: its number.
fn transcript_of(wav: &[u8]) -> anyhow::Result<String> {
    Ok(u64::from_le_bytes(wav.try_into()?).to_string())
}

/// Answers a transcription request that the owner may already have abandoned.
#[expect(
    clippy::let_underscore_must_use,
    reason = "An abandoned request has nobody left to answer"
)]
fn answer_unless_abandoned(reply: Reply, result: anyhow::Result<String>) {
    let _ = reply.send(result);
}

/// Resolves once every task has parked: on the paused clock, the sleep ends only when nothing else
/// can run.
async fn settle() {
    tokio::time::sleep(Duration::from_micros(1)).await;
}

/// A device between its opening and its release.
#[derive(Debug)]
struct Open {
    id: SessionId,
    retiring: bool,
}

/// A device the owner asked to finish, or whose outcome the test sent first.
struct Ending {
    device: (Source, SessionId),
    /// Whether the test has sent what the device recorded.
    answered: bool,
}

/// The owner under test, and what the model ports reported so far.
struct Model {
    runtime: Runtime<OwnedTask>,
    updates: watch::Receiver<Snapshot>,
    log: Receiver<Log>,
    jobs: Receiver<Job>,
    /// Transcription requests not yet answered, oldest first, with the transcript of their speech.
    replies: VecDeque<(String, Reply)>,
    microphone: Option<(SessionId, Sender<CaptureEvent>)>,
    screen: Option<(SessionId, Sender<ShowEvent>)>,
    open: HashMap<Source, Open>,
    finished: Vec<Ending>,
    /// Whether a speech worker has started and not yet stopped.
    worker: bool,
}

impl Model {
    fn start(releases: Releases, faults: &[bool]) -> anyhow::Result<Self> {
        let (log, logged) = async_channel::unbounded();
        let (jobs, requested) = async_channel::unbounded();
        let (fault, faults_in_turn) = async_channel::unbounded();
        for &fails in faults {
            fault.try_send(fails)?;
        }
        let (output, updates) = watch::channel(Snapshot::default());
        let ports = ModelPorts {
            log,
            jobs,
            faults: faults_in_turn,
            presented: updates.clone(),
            releases,
        };
        let (meter, _) = watch::channel(Meter::default());
        let runtime = Runtime::on_task(Config::default(), output, meter, ports)?;
        Ok(Self {
            runtime,
            updates,
            log: logged,
            jobs: requested,
            replies: VecDeque::new(),
            microphone: None,
            screen: None,
            open: HashMap::new(),
            finished: Vec::new(),
            worker: false,
        })
    }

    async fn step(&mut self, step: &Step) -> anyhow::Result<()> {
        match step {
            Step::Inputs(keys) => {
                for key in keys {
                    self.runtime.input.deliver(key.input());
                }
            },
            Step::Advance(by) => tokio::time::advance(*by).await,
            Step::Audio(heard) => {
                if let Some((id, events)) = &self.microphone {
                    let outcome = match heard {
                        Heard::Speech => Ok(Some(spoken_by(*id))),
                        Heard::Silence => Ok(None),
                        Heard::Failure => Err(anyhow!("Model microphone failed")),
                    };
                    events.try_send(CaptureEvent::Finished(*id, outcome))?;
                    self.answered(Source::Microphone, *id);
                }
            },
            Step::Footage(seen) => {
                if let Some((id, events)) = &self.screen {
                    let event = match seen {
                        Seen::Complete => self
                            .finishing(Source::Screen, *id)
                            .then(|| ShowEvent::Finished(*id, Ok(footage()))),
                        Seen::Failure => Some(ShowEvent::Finished(
                            *id,
                            Err(anyhow!("Model screen failed")),
                        )),
                        Seen::Unchosen => Some(ShowEvent::Unchosen(*id, ports::Unchosen::Area)),
                    };
                    if let Some(event) = event {
                        events.try_send(event)?;
                        self.answered(Source::Screen, *id);
                    }
                }
            },
            Step::Answer(said) => {
                if let Some((transcript, reply)) = self.replies.pop_front() {
                    let result = match said {
                        Said::Words => Ok(transcript),
                        Said::Nothing => Ok(String::new()),
                        Said::Failure => Err(anyhow!("Model transcription failed")),
                    };
                    answer_unless_abandoned(reply, result);
                }
            },
            Step::Configure(setting) => {
                let mut config = self.runtime.configuration.borrow().clone();
                match setting {
                    Setting::Fillers => config.remove_fillers = !config.remove_fillers,
                    Setting::Gpu => config.use_gpu = !config.use_gpu,
                }
                self.runtime.configure(ValidConfig::assume(config));
            },
        }
        settle().await;
        self.absorb()?;
        self.check()
    }

    /// Answers everything outstanding and jumps past the limit until nothing changes, then requires
    /// every session to have ended with its devices released.
    async fn drain(&mut self) -> anyhow::Result<()> {
        for _ in 0..DRAIN_ROUNDS {
            let mut acted = false;
            while let Some((transcript, reply)) = self.replies.pop_front() {
                answer_unless_abandoned(reply, Ok(transcript));
                acted = true;
            }
            for (source, id) in self.unanswered() {
                self.complete(source, id)?;
                // Answers to ended sessions still occupy the owner's four-event Show lane, so the
                // owner takes each one before the next.
                settle().await;
                acted = true;
            }
            tokio::time::advance(PAST_THE_LIMIT).await;
            settle().await;
            acted |= self.absorb()?;
            self.check()?;
            if !acted {
                let phase = self.updates.borrow().phase;
                ensure!(!phase.is_active(), "The owner never left {phase:?}");
                ensure!(self.open.is_empty(), "Nothing released {:?}", self.open);
                return Ok(());
            }
        }
        bail!("The owner kept working through {DRAIN_ROUNDS} drain rounds")
    }

    /// Sends what `source` recorded for `id` as a complete recording.
    fn complete(&mut self, source: Source, id: SessionId) -> anyhow::Result<()> {
        match (source, &self.microphone, &self.screen) {
            (Source::Microphone, Some((_, events)), _) => {
                events.try_send(CaptureEvent::Finished(id, Ok(Some(spoken_by(id)))))?;
            },
            (Source::Screen, _, Some((_, events))) => {
                events.try_send(ShowEvent::Finished(id, Ok(footage())))?;
            },
            _ => bail!("{source:?} finished for {id:?} without opening"),
        }
        self.answered(source, id);
        Ok(())
    }

    /// Stops the owner at rest and joins it, surfacing any panic, a release of a device that was
    /// not open, or a worker left running. The scripted scenarios cover what shutdown releases from
    /// a live session.
    async fn close(mut self) -> anyhow::Result<()> {
        self.runtime.close().await?;
        self.absorb()?;
        ensure!(!self.worker, "Shutdown left a worker running");
        Ok(())
    }

    /// Takes in what the ports logged, checking that a device opens only once its previous session
    /// released it and releases only after the owner retired it, that text is inserted only while
    /// the session that spoke it is the latest, and that a worker starts only once the previous one
    /// stopped and is never dropped unstopped; returns whether anything arrived.
    fn absorb(&mut self) -> anyhow::Result<bool> {
        let mut arrived = false;
        while let Ok(entry) = self.log.try_recv() {
            arrived = true;
            match entry {
                Log::MicrophoneOpened(id, events) => {
                    self.opened(Source::Microphone, id)?;
                    self.microphone = Some((id, events));
                },
                Log::ScreenOpened(id, events) => {
                    self.opened(Source::Screen, id)?;
                    self.screen = Some((id, events));
                },
                Log::Finished(source, id) => {
                    if !self.finishing(source, id) {
                        self.finished.push(Ending {
                            device: (source, id),
                            answered: false,
                        });
                    }
                },
                Log::Retiring(source, id) => match self.open.get_mut(&source) {
                    Some(open) if open.id == id => open.retiring = true,
                    _ => bail!("{source:?} {id:?} retired without being open"),
                },
                Log::Retired(source, id) => {
                    let released = self.open.remove(&source);
                    ensure!(
                        released.is_some_and(|open| open.id == id && open.retiring),
                        "{source:?} {id:?} released without being retired"
                    );
                },
                Log::Pinned(id) => ensure!(
                    self.open
                        .get(&Source::Screen)
                        .is_some_and(|open| open.id == id && !open.retiring),
                    "Screen {id:?} took a pin while not recording a Show"
                ),
                Log::Inserted { text, latest } => ensure!(
                    text == latest.to_string(),
                    "Session {text}'s text was inserted after session {latest} began"
                ),
                Log::Loaded => {
                    ensure!(
                        !self.worker,
                        "A worker started before the previous one stopped"
                    );
                    self.worker = true;
                },
                Log::Stopped => {
                    ensure!(self.worker, "A worker stopped without having started");
                    self.worker = false;
                },
                Log::Abandoned => bail!("A worker was dropped without being stopped"),
            }
        }
        while let Ok((wav, reply)) = self.jobs.try_recv() {
            arrived = true;
            self.replies.push_back((transcript_of(&wav)?, reply));
        }
        Ok(arrived)
    }

    fn opened(&mut self, source: Source, id: SessionId) -> anyhow::Result<()> {
        let open = Open {
            id,
            retiring: false,
        };
        if let Some(previous) = self.open.insert(source, open) {
            bail!(
                "{source:?} opened for {id:?} before {:?} released it",
                previous.id
            );
        }
        Ok(())
    }

    /// The owner still runs, and once no session is active it has retired every device.
    fn check(&self) -> anyhow::Result<()> {
        ensure!(
            !self.runtime.host.task.is_finished(),
            "The owner stopped on its own"
        );
        let phase = self.updates.borrow().phase;
        if !phase.is_active()
            && let Some((source, open)) = self.open.iter().find(|(_, open)| !open.retiring)
        {
            bail!(
                "{source:?} stayed open for {:?} after the session ended in {phase:?}",
                open.id
            );
        }
        Ok(())
    }

    fn finishing(&self, source: Source, id: SessionId) -> bool {
        self.finished
            .iter()
            .any(|ending| ending.device == (source, id))
    }

    fn unanswered(&self) -> Vec<(Source, SessionId)> {
        self.finished
            .iter()
            .filter(|ending| !ending.answered)
            .map(|ending| ending.device)
            .collect()
    }

    /// Records that the test sent `source`'s outcome for `id`, finished or not.
    fn answered(&mut self, source: Source, id: SessionId) {
        let device = (source, id);
        match self
            .finished
            .iter_mut()
            .find(|ending| ending.device == device)
        {
            Some(ending) => ending.answered = true,
            None => self.finished.push(Ending {
                device,
                answered: true,
            }),
        }
    }
}

async fn run(steps: &[Step], releases: Releases, faults: &[bool]) -> anyhow::Result<()> {
    let mut model = Model::start(releases, faults)?;
    settle().await;
    model.absorb()?;
    for step in steps {
        model.step(step).await?;
    }
    model.drain().await?;
    model.close().await
}

fn paused() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn the_owner_releases_every_device_and_comes_to_rest(
        steps in prop::collection::vec(step(), 0..=40),
        releases in releases(),
        faults in prop::collection::vec(prop::bool::weighted(0.3), 0..=6),
    ) {
        paused()?
            .block_on(run(&steps, releases, &faults))
            .map_err(|error| TestCaseError::fail(format!("{error:#}")))?;
    }
}

/// Sequences the property once shrank failures to, kept as cases of their own because any change
/// to the strategy changes what a saved seed generates.
#[test]
fn the_sequences_that_once_stranded_the_owner_come_to_rest() -> anyhow::Result<()> {
    let show = || Step::Inputs(SHOW_GESTURE.to_vec());
    let keys = |keys: &[Key]| Step::Inputs(keys.to_vec());
    let at_once = Releases {
        microphone: Duration::ZERO,
        screen: Duration::ZERO,
        worker: Duration::ZERO,
    };
    let speech = || Step::Audio(Heard::Speech);
    let microphone_failure = || Step::Audio(Heard::Failure);
    let screen_failure = || Step::Footage(Seen::Failure);
    let cases = [
        (vec![keys(&[Key::Toggle]), microphone_failure()], at_once),
        (vec![show(), microphone_failure()], at_once),
        (
            vec![
                show(),
                show(),
                speech(),
                screen_failure(),
                Step::Advance(Duration::from_millis(141)),
                Step::Advance(Duration::ZERO),
                show(),
                speech(),
            ],
            at_once,
        ),
        (
            vec![
                show(),
                keys(&[Key::Cancel]),
                keys(&[Key::Press, Key::Show]),
                speech(),
            ],
            Releases {
                screen: Duration::from_millis(2),
                ..at_once
            },
        ),
        (
            vec![
                show(),
                show(),
                microphone_failure(),
                show(),
                show(),
                keys(&[Key::Cancel]),
                show(),
                screen_failure(),
                show(),
                keys(&[Key::Press]),
                microphone_failure(),
                show(),
                show(),
                show(),
                microphone_failure(),
                show(),
                show(),
            ],
            at_once,
        ),
    ];
    for (steps, releases) in cases {
        paused()?.block_on(run(&steps, releases, &[]))?;
    }
    Ok(())
}
