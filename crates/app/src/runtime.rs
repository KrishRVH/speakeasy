mod device;
mod owner;
pub(crate) mod ports;
mod recognition;
mod session;
mod worker;

use std::{
    convert::Infallible,
    io,
    sync::Arc,
    time::{Instant, SystemTime},
};

use async_channel::{Receiver, Sender, TrySendError};
use speakeasy_platform::{
    Delivery, Input, InputMonitor, InputSender, InsertPermit, Inserted, Inserter, OwnedThread,
    Scope,
};
use speakeasy_show::{Ending, NarrationLoss};
use tokio::{
    sync::{mpsc, watch},
    task::JoinError,
};
use zeroize::Zeroizing;

use self::ports::{CaptureEvent, Ports, Published, RecognitionMode, SessionId, ShowEvent};
use crate::{
    audio::Capture,
    config::{Change, Config, ValidConfig},
    control::{Call, Reply},
    local_speech::LocalSpeech,
    show::{self, Canvas, Interrupted, SavedShow, ScreenRecorder},
    sounds::{Cue, Sounds},
    transcript::style::Style,
};

pub(crate) const LOADING: &str = "Loading local model…";

const SHOW_COPIED: &str = "Show copied. Paste it into your coding agent.";

const SHOW_GONE: &str = "The last Show has expired or was deleted.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Idle,
    Starting,
    Recording,
    Stopping,
    Processing,
    Done,
    Copied,
    Notice,
    Empty,
    Cancelled,
    Unchosen,
    Error,
}

impl Phase {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Recording => "recording",
            Self::Stopping => "stopping",
            Self::Processing => "processing",
            Self::Done => "done",
            Self::Copied => "copied",
            Self::Notice => "notice",
            Self::Empty => "empty",
            Self::Cancelled => "cancelled",
            Self::Unchosen => "unchosen",
            Self::Error => "error",
        }
    }

    pub(crate) const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Recording | Self::Stopping | Self::Processing
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelState {
    Loading,
    Ready,
    Unavailable,
}

impl ModelState {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SnapshotEpoch(u64);

impl SnapshotEpoch {
    pub(crate) const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "The immutable UI snapshot reports independent session, desktop and retained-text facts; it owns no transitions"
)]
pub(crate) struct Snapshot {
    pub epoch: SnapshotEpoch,
    pub session: u64,
    pub reported: u64,
    pub phase: Phase,
    pub hands_free: bool,
    pub show: bool,
    pub ending: Ending,
    pub narration_lost: Option<NarrationLoss>,
    pub started: Instant,
    pub last_show: Option<SavedShow>,
    pub last_dictation: bool,
    pub style: Style,
    pub preset: Option<String>,
    pub message: String,
    pub model: ModelState,
    pub desktop_ready: bool,
    pub shortcut: Arc<str>,
    pub cancel_shortcut: Arc<str>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            epoch: SnapshotEpoch::default(),
            session: 0,
            reported: 0,
            phase: Phase::Idle,
            hands_free: false,
            show: false,
            ending: Ending::Finished,
            narration_lost: None,
            started: now(),
            last_show: None,
            last_dictation: false,
            style: Style::default(),
            preset: None,
            message: String::new(),
            model: ModelState::Unavailable,
            desktop_ready: true,
            shortcut: speakeasy_platform::SHORTCUT.into(),
            cancel_shortcut: speakeasy_platform::CANCEL_SHORTCUT.into(),
        }
    }
}

pub(crate) enum Report {
    Failed(String),
    Recurring(String),
    #[cfg_attr(
        all(target_os = "linux", not(test)),
        expect(
            dead_code,
            reason = "Linux records no Shows, so its menu never copies one"
        )
    )]
    ShowCopied,
    #[cfg_attr(
        all(target_os = "linux", not(test)),
        expect(
            dead_code,
            reason = "Linux records no Shows, so its menu never looks for one"
        )
    )]
    ShowGone(SavedShow),
}

impl Report {
    pub(crate) fn present(self, snapshot: &mut Snapshot) -> bool {
        let shown = snapshot.clone();
        self.forget_gone(&mut snapshot.last_show);
        if snapshot.phase.is_active() {
            return *snapshot != shown;
        }
        if self.presents_anew() {
            snapshot.reported = snapshot.reported.wrapping_add(1);
        }
        (snapshot.phase, snapshot.message) = match self {
            Self::Failed(message) | Self::Recurring(message) => (Phase::Error, message),
            Self::ShowCopied => {
                (snapshot.show, snapshot.ending, snapshot.narration_lost) =
                    (true, Ending::Finished, None);
                (Phase::Copied, SHOW_COPIED.to_owned())
            },
            Self::ShowGone(_) => (Phase::Notice, SHOW_GONE.to_owned()),
        };
        *snapshot != shown
    }

    fn presents_anew(&self) -> bool {
        !matches!(self, Self::Recurring(_))
    }

    fn forget_gone(&self, last_show: &mut Option<SavedShow>) {
        if let Self::ShowGone(gone) = self
            && last_show.as_ref() == Some(gone)
        {
            *last_show = None;
        }
    }
}

impl From<&anyhow::Error> for Report {
    fn from(error: &anyhow::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Meter {
    pub epoch: SnapshotEpoch,
    pub session: u64,
    pub level: f32,
}

pub(crate) struct Runtime<Host = OwnedThread> {
    host: Host,
    pub input: InputSender,
    configuration: watch::Sender<Config>,
    reports: Sender<Report>,
    recoveries: Sender<Recovery>,
    practices: Sender<mpsc::Sender<PracticeEvent>>,
    commands: Sender<Directive>,
    stop: Sender<Infallible>,
}

struct Configuration {
    effective: Config,
    saved: Config,
    styles: watch::Receiver<Style>,
}

impl Runtime {
    pub(crate) fn start(
        config: ValidConfig,
        baseline: Config,
        styles: watch::Receiver<Style>,
        snapshots: watch::Sender<Snapshot>,
        meter: watch::Sender<Meter>,
        canvas: watch::Sender<Canvas>,
    ) -> anyhow::Result<(Self, InputMonitor)> {
        let (input, inputs) = input_lane();
        let (monitor, inserter) =
            speakeasy_platform::prepare(input.clone(), config.desktop_options())?;
        let ports = Desktop {
            inserter,
            canvas,
            sounds: Sounds::start()?,
            mac_shortcut: config.mac_shortcut,
        };
        let runtime = Self::host(
            Configuration {
                effective: config.into_inner(),
                saved: baseline,
                styles,
            },
            snapshots,
            meter,
            ports,
            input,
            inputs,
            Wiring::run_on_own_thread,
        )?;
        Ok((runtime, monitor))
    }

    #[cfg(test)]
    fn start_with<P: Ports>(
        config: Config,
        styles: watch::Receiver<Style>,
        snapshots: watch::Sender<Snapshot>,
        meter: watch::Sender<Meter>,
        ports: P,
    ) -> anyhow::Result<Self> {
        let (input, inputs) = input_lane();
        Self::host(
            Configuration {
                effective: config.clone(),
                saved: config,
                styles,
            },
            snapshots,
            meter,
            ports,
            input,
            inputs,
            Wiring::run_on_own_thread,
        )
    }

    pub(crate) fn is_running(&self) -> bool {
        !self.input.is_closed() && !self.host.is_finished()
    }

    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + use<> {
        self.host.exited()
    }
}

impl<Host> Runtime<Host> {
    fn host<P: Ports>(
        configuration: Configuration,
        snapshots: watch::Sender<Snapshot>,
        meter: watch::Sender<Meter>,
        ports: P,
        input: InputSender,
        inputs: Receiver<Input>,
        run: impl FnOnce(Wiring<P>) -> io::Result<Host>,
    ) -> anyhow::Result<Self> {
        let (epoch, last_show) = {
            let presented = snapshots.borrow();
            (presented.epoch, presented.last_show.clone())
        };
        let Configuration {
            effective,
            saved,
            styles,
        } = configuration;
        let incarnation = getrandom::u64()?;
        let (configuration, changes) = watch::channel(saved);
        let (commands, directed) = async_channel::bounded(8);
        let (reports, reported) = async_channel::unbounded();
        let (stop, stopping) = async_channel::bounded(1);
        let (recoveries, recovering) = async_channel::bounded(1);
        let (practices, practicing) = async_channel::bounded(1);
        let host = run(Wiring {
            config: effective,
            changes,
            styles,
            commands: directed,
            incarnation,
            inputs,
            reports: reported,
            recoveries: recovering,
            practices: practicing,
            snapshots,
            meter,
            permits: input.clone(),
            stopping,
            ports,
            epoch,
            last_show,
        })?;
        Ok(Self {
            host,
            input,
            configuration,
            reports,
            recoveries,
            practices,
            commands,
            stop,
        })
    }

    pub(crate) fn report(&self, report: Report) -> Result<(), Report> {
        self.reports
            .try_send(report)
            .map_err(TrySendError::into_inner)
    }

    pub(crate) fn recover(
        &self,
        interrupted: Interrupted,
    ) -> Result<RecoveryReceipt, &'static str> {
        let (completed, result) = async_channel::bounded(1);
        self.recoveries
            .try_send(Recovery {
                interrupted,
                completed,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => "A Show recovery is already waiting.",
                TrySendError::Closed(_) => "Resume Speakeasy to recover this Show.",
            })?;
        Ok(result)
    }

    pub(crate) fn practice(&self) -> Result<mpsc::Receiver<PracticeEvent>, &'static str> {
        let (sender, receipt) = mpsc::channel(2);
        self.practices
            .try_send(sender)
            .map_err(|error| match error {
                TrySendError::Full(_) => "Practice is already starting.",
                TrySendError::Closed(_) => "Resume dictation to try it here.",
            })?;
        Ok(receipt)
    }

    pub(crate) fn configure(&self, config: ValidConfig) {
        if self.configuration.borrow().change_to(&config) < Change::Session {
            return;
        }

        self.input.cancel();
        self.configuration.send_replace(config.into_inner());
    }

    pub(crate) fn control(&self, call: Call) {
        self.direct(Directive::Control(call));
    }

    pub(crate) fn preset(
        &self,
        name: String,
        config: ValidConfig,
        baseline: Config,
        session: u64,
        call: Call,
    ) {
        self.direct(Directive::Preset {
            name,
            config: Box::new(config),
            baseline: Box::new(baseline),
            session,
            call,
        });
    }

    fn direct(&self, directive: Directive) {
        if let Err(error) = self.commands.try_send(directive) {
            let reason = if error.is_full() {
                "Speakeasy is busy. Try the command again."
            } else {
                "Resume Speakeasy before using this command."
            };
            error.into_inner().call().respond(Reply::refused(reason));
        }
    }

    pub(crate) fn request_stop(&self) {
        self.stop.close();
        self.input.close();
    }
}

impl<Host> Drop for Runtime<Host> {
    fn drop(&mut self) {
        self.request_stop();
    }
}

struct Desktop {
    inserter: Inserter,
    canvas: watch::Sender<Canvas>,
    sounds: Sounds,
    mac_shortcut: speakeasy_platform::MacShortcut,
}

impl Ports for Desktop {
    type Microphone = Capture;
    type Screen = ScreenRecorder;
    type Speech = LocalSpeech;

    fn prepares_desktop(&self) -> bool {
        true
    }

    fn record_microphone(
        &self,
        id: SessionId,
        microphone: Option<&str>,
        recognition: RecognitionMode,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Capture> {
        Capture::start(id, microphone, recognition, events)
    }

    fn keep(&self, recorded: SystemTime) -> anyhow::Result<Option<show::Kept>> {
        show::keep(recorded).map(Some)
    }

    fn record_screen(
        &self,
        id: SessionId,
        started: Instant,
        scope: Scope,
        log: Option<show::FrameLog>,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<ScreenRecorder> {
        ScreenRecorder::start(
            id,
            started,
            scope,
            log,
            events,
            self.canvas.clone(),
            self.mac_shortcut,
        )
    }

    fn publish(
        &self,
        publication: show::Publication,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        show::publish(publication, permit)
    }

    fn reopen(
        &self,
        interrupted: Interrupted,
    ) -> impl Future<Output = anyhow::Result<show::Reopened>> + Send + 'static {
        show::reopen(interrupted)
    }

    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<LocalSpeech>> + Send + 'static {
        LocalSpeech::start(config, cancelled)
    }

    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        delivery: Delivery,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let inserter = self.inserter.clone();
        async move { inserter.insert(text, permit, delivery).await }
    }

    fn cue(&self, cue: Cue) {
        self.sounds.play(cue);
    }

    fn hold_cues(&self, held: bool) {
        self.sounds.hold(held);
    }
}

pub(crate) type RecoveryReceipt = Receiver<Result<SavedShow, String>>;

pub(crate) enum PracticeEvent {
    Ready,
    Text(Zeroizing<String>),
    Unavailable(&'static str),
}

struct Recovery {
    interrupted: Interrupted,
    completed: Sender<Result<SavedShow, String>>,
}

struct Wiring<P> {
    config: Config,
    changes: watch::Receiver<Config>,
    styles: watch::Receiver<Style>,
    commands: Receiver<Directive>,
    incarnation: u64,
    inputs: Receiver<Input>,
    reports: Receiver<Report>,
    recoveries: Receiver<Recovery>,
    practices: Receiver<mpsc::Sender<PracticeEvent>>,
    snapshots: watch::Sender<Snapshot>,
    meter: watch::Sender<Meter>,
    permits: InputSender,
    stopping: Receiver<Infallible>,
    ports: P,
    epoch: SnapshotEpoch,
    last_show: Option<SavedShow>,
}

enum Directive {
    Control(Call),
    Preset {
        name: String,
        config: Box<ValidConfig>,
        baseline: Box<Config>,
        session: u64,
        call: Call,
    },
}

impl Directive {
    fn call(self) -> Call {
        match self {
            Self::Control(call) | Self::Preset { call, .. } => call,
        }
    }
}

impl<P: Ports> Wiring<P> {
    fn run_on_own_thread(self) -> io::Result<OwnedThread> {
        OwnedThread::spawn("dictation", move || self.run_on_current_thread())
    }

    fn run_on_current_thread(self) {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(owner::run(self)),
            Err(error) => self.fail_startup(&error),
        }
    }

    fn fail_startup(&self, error: &io::Error) {
        self.snapshots.send_if_modified(|snapshot| {
            if snapshot.epoch != self.epoch {
                return false;
            }
            snapshot.phase = Phase::Error;
            snapshot.message =
                format!("Could not start dictation ({error}). Quit and reopen Speakeasy.");
            true
        });
    }
}

pub(crate) fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

fn input_lane() -> (InputSender, Receiver<Input>) {
    let (sender, inputs) = async_channel::bounded(64);
    (InputSender::new(sender), inputs)
}

async fn reap<T>(task: impl Future<Output = Result<T, JoinError>>) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A reaped task's result or panic belongs to no current session; awaiting it still completes its owned cleanup"
    )]
    let _ = task.await;
}

#[cfg(test)]
mod fixture;
#[cfg(test)]
mod tests;
