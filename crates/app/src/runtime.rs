mod device;
mod native;
mod owner;
pub(crate) mod ports;
mod worker;

use std::{
    convert::Infallible,
    io,
    sync::Arc,
    time::{Instant, SystemTime},
};

use async_channel::{Receiver, Sender, TrySendError};
use speakeasy_core::machine::{Cue, ModelState, Phase, RecognitionMode, SessionId};
use speakeasy_platform::{
    Delivery, Enter, Input, InputMonitor, InputSender, InsertPermit, Inserted, Inserter, Keep,
    OwnFocus, OwnedThread, Scope,
};
use speakeasy_show::{Ending, NarrationLoss};
use tokio::sync::{mpsc, oneshot, watch};

use self::ports::{CaptureEvent, Ports, Published, ShowEvent};
use crate::{
    audio::Capture,
    config::{Change, Config, ValidConfig},
    control::{Call, Reply},
    local_speech::LocalSpeech,
    show::{self, Canvas, Interrupted, SavedShow, ScreenRecorder},
    sounds::Sounds,
    transcript::style::Style,
};

pub(crate) const LOADING: &str = "Loading speech model…";

const SHOW_COPIED: &str = "Show copied. Paste it into your coding agent.";

const SHOW_GONE: &str = "The last Show has expired or was deleted.";

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "The immutable UI snapshot reports independent session, desktop and retained-text facts; it owns no transitions"
)]
pub(crate) struct Snapshot {
    pub session: u64,
    /// Counts reports shown outside a session, so a repeated report still reads as new.
    ///
    /// A `Recurring` report does not advance it.
    pub reported: u64,
    pub phase: Phase,
    pub hands_free: bool,
    pub for_script: bool,
    pub default_microphone: bool,
    pub show: bool,
    pub ending: Ending,
    pub narration_lost: Option<NarrationLoss>,
    pub started: Instant,
    /// Outlives each owner: announcements keep it and a new owner starts from it.
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
            session: 0,
            reported: 0,
            phase: Phase::Idle,
            hands_free: false,
            for_script: false,
            default_microphone: false,
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

    /// False for `Recurring`, so the hourly retention sweep cannot pop the same failure up again.
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

pub(crate) struct Runtime<Host = OwnedThread> {
    host: Host,
    pub input: InputSender,
    baseline: Config,
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
    #[expect(
        clippy::too_many_arguments,
        reason = "The service owner hands over each lane and handle the desktop runtime uses; a parameter struct would only rename this list"
    )]
    pub(crate) fn start(
        config: ValidConfig,
        baseline: Config,
        styles: watch::Receiver<Style>,
        snapshots: watch::Sender<Snapshot>,
        meter: watch::Sender<f32>,
        canvas: watch::Sender<Canvas>,
        wsl: show::wsl::Lookup,
        own_focus: OwnFocus,
    ) -> anyhow::Result<(Self, InputMonitor)> {
        let (input, inputs) = input_lane();
        let (monitor, inserter) =
            speakeasy_platform::prepare(input.clone(), config.desktop_options(), own_focus)?;
        let ports = Desktop {
            inserter,
            canvas,
            wsl,
            sounds: Sounds::start()?,
            shortcut: config.shortcut(),
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
        meter: watch::Sender<f32>,
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
        meter: watch::Sender<f32>,
        ports: P,
        input: InputSender,
        inputs: Receiver<Input>,
        run: impl FnOnce(Wiring<P>) -> io::Result<Host>,
    ) -> anyhow::Result<Self> {
        let Configuration {
            effective,
            saved,
            styles,
        } = configuration;
        let incarnation = getrandom::u64()?;
        let (commands, directed) = async_channel::unbounded();
        let (reports, reported) = async_channel::unbounded();
        let (stop, stopping) = async_channel::bounded(1);
        let (recoveries, recovering) = async_channel::bounded(1);
        let (practices, practicing) = async_channel::bounded(1);
        let host = run(Wiring {
            config: effective,
            baseline: saved.clone(),
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
        })?;
        Ok(Self {
            host,
            input,
            baseline: saved,
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
        let (completed, receipt) = oneshot::channel();
        self.recoveries
            .try_send(Recovery {
                interrupted,
                completed,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => "A Show recovery is already waiting.",
                TrySendError::Closed(_) => "Resume Speakeasy to recover this Show.",
            })?;
        Ok(receipt)
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

    pub(crate) const fn baseline(&self) -> &Config {
        &self.baseline
    }

    pub(crate) fn configure(&mut self, config: ValidConfig) {
        let config = config.into_inner();
        if reads_differently(&self.baseline, &config) {
            self.baseline.clone_from(&config);
            // Revoke now, so a preparing insertion cannot commit under the old settings.
            self.input.cancel();
            self.direct(Directive::Apply(Box::new(config)));
        } else {
            self.revert(config);
        }
    }

    pub(crate) fn adopt(&self, config: ValidConfig) -> Receiver<Adopted> {
        let (answer, receipt) = async_channel::bounded(1);
        self.direct(Directive::Adopt {
            config: Box::new(config.into_inner()),
            answer: Adopting(answer),
        });
        receipt
    }

    pub(crate) fn revert(&mut self, saved: Config) -> bool {
        let changed = self.baseline.change_to(&saved) >= Change::Session;
        self.baseline.clone_from(&saved);
        if changed {
            self.direct(Directive::Revert(Box::new(saved)));
        }
        changed
    }

    pub(crate) fn settle(&mut self, config: ValidConfig) {
        self.baseline = config.into_inner();
    }

    pub(crate) fn restart_when_idle(&self, restart: Sender<()>) {
        self.direct(Directive::Restart(Restarting(restart)));
    }

    pub(crate) fn control(&self, call: Call) {
        self.direct(Directive::Control(call));
    }

    pub(crate) fn preset(&self, name: String, config: ValidConfig, call: Call) {
        self.direct(Directive::Preset {
            name,
            config: Box::new(config),
            call,
        });
    }

    fn direct(&self, directive: Directive) {
        if let Err(error) = self.commands.try_send(directive) {
            error
                .into_inner()
                .refuse("Resume Speakeasy before using this command.");
        }
    }

    /// Wakes the owner on its stop lane before closing input.
    ///
    /// The owner's biased `select!` then sees Stop first, so a normal pause never reports a
    /// shortcut failure or drains queued presses.
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

fn reads_differently(baseline: &Config, config: &Config) -> bool {
    let rebound = Config {
        linux: config.linux.clone(),
        mac_shortcut: config.mac_shortcut,
        windows_shortcut: config.windows_shortcut,
        ..baseline.clone()
    };
    rebound.change_to(config) >= Change::Session
}

struct Desktop {
    inserter: Inserter,
    canvas: watch::Sender<Canvas>,
    wsl: show::wsl::Lookup,
    sounds: Sounds,
    shortcut: speakeasy_platform::Shortcut,
}

impl Ports for Desktop {
    type Awake = speakeasy_platform::Awake;
    type Microphone = Capture;
    type Screen = ScreenRecorder;
    type Speech = LocalSpeech;

    fn prepares_desktop(&self) -> bool {
        true
    }

    fn keep_awake(&self, keep: Keep) -> Option<Self::Awake> {
        speakeasy_platform::Awake::hold(keep)
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
        self.wsl.prepare();
        ScreenRecorder::start(
            id,
            started,
            scope,
            log,
            events,
            self.canvas.clone(),
            self.shortcut,
        )
    }

    fn publish(
        &self,
        publication: show::Publication,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        show::publish(publication, permit, self.wsl.clone())
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
        enter: Enter,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let inserter = self.inserter.clone();
        async move { inserter.insert(text, permit, delivery, enter).await }
    }

    fn cue(&self, cue: Cue) {
        self.sounds.play(cue);
    }

    fn hold_cues(&self, held: bool) {
        self.sounds.hold(held);
    }
}

pub(crate) type RecoveryReceipt = oneshot::Receiver<RecoveryResult>;

type RecoveryResult = Result<SavedShow, String>;

/// Deliberately has no `Debug`, so recognized practice words cannot reach a log.
pub(crate) enum PracticeEvent {
    Ready,
    Text(String),
    Unavailable(&'static str),
}

struct Recovery {
    interrupted: Interrupted,
    completed: oneshot::Sender<RecoveryResult>,
}

struct Wiring<P> {
    config: Config,
    baseline: Config,
    styles: watch::Receiver<Style>,
    commands: Receiver<Directive>,
    incarnation: u64,
    inputs: Receiver<Input>,
    reports: Receiver<Report>,
    recoveries: Receiver<Recovery>,
    practices: Receiver<mpsc::Sender<PracticeEvent>>,
    snapshots: watch::Sender<Snapshot>,
    meter: watch::Sender<f32>,
    permits: InputSender,
    stopping: Receiver<Infallible>,
    ports: P,
}

pub(crate) type Adopted = Result<(), &'static str>;

enum Directive {
    Control(Call),
    Preset {
        name: String,
        config: Box<ValidConfig>,
        call: Call,
    },
    Adopt {
        config: Box<Config>,
        answer: Adopting,
    },
    Apply(Box<Config>),
    Revert(Box<Config>),
    Restart(Restarting),
}

impl Directive {
    fn refuse(self, reason: &'static str) {
        match self {
            Self::Control(call) | Self::Preset { call, .. } => call.respond(Reply::refused(reason)),
            Self::Adopt { answer, .. } => answer.answer(Err(reason)),
            Self::Apply(_) | Self::Revert(_) | Self::Restart(_) => (),
        }
    }
}

struct Restarting(Sender<()>);

impl Restarting {
    fn restart(self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Services stops waiting for a restart once it pauses or starts another owner"
        )]
        let _ = self.0.try_send(());
    }
}

struct Adopting(Sender<Adopted>);

impl Adopting {
    fn is_live(&self) -> bool {
        !self.0.is_closed()
    }

    fn answer(self, adopted: Adopted) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Services stops waiting for a Use only when Quit cancels its preset tasks"
        )]
        let _ = self.0.try_send(adopted);
    }
}

impl<P: Ports> Wiring<P> {
    fn run_on_own_thread(self) -> io::Result<OwnedThread> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        OwnedThread::spawn("dictation", move || runtime.block_on(owner::run(self)))
    }
}

/// Follows tokio's paused clock in tests, so deadlines and snapshots share one timeline.
pub(crate) fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

fn input_lane() -> (InputSender, Receiver<Input>) {
    let (sender, inputs) = async_channel::bounded(64);
    (InputSender::new(sender), inputs)
}

#[cfg(test)]
mod fixture;
#[cfg(test)]
mod tests;
