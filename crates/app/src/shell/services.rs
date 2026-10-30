//! UI-thread ownership of native services. Cleanup waits on acknowledgements, retaining
//! thread-affine adapters here until their native work has stopped.

use std::path::PathBuf;

use anyhow::anyhow;
use gpui::{App, BorrowAppContext, Global, Task, WindowHandle};
use speakeasy_platform::{Input, InputMonitor};
use tokio::sync::watch;

use super::{
    lifecycle::{ConfigEpoch, Lifecycle},
    settings::Settings,
    window::{self, reveal},
};
#[cfg(any(target_os = "windows", target_os = "macos", test))]
use crate::show::SavedShow;
use crate::{
    audio::MicrophoneScan,
    config::{Change, Config, ValidConfig},
    gpui_ext::AppUpdate,
    instance::{Instance, Request},
    pill::Pill,
    runtime::{self, Meter, ModelState, Phase, Report, Runtime, Snapshot},
    show::{Canvas, review::Review},
    status::{PAUSED, PAUSING, QUITTING, ServiceState, Status},
};

/// The status while a resume checks the saved settings, before any owner exists.
const CHECKING: &str = "Checking local speech settings…";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchMode {
    Live,
    Demo,
    DemoTray,
}

impl LaunchMode {
    pub(crate) const fn is_demo(self) -> bool {
        matches!(self, Self::Demo | Self::DemoTray)
    }

    /// A resident app owns the single instance and a tray icon, and closing Settings hides it.
    pub(crate) const fn is_resident(self) -> bool {
        matches!(self, Self::Live | Self::DemoTray)
    }
}

/// What starts the service again once retiring owners acknowledge.
enum Restart {
    /// Settings a save made durable.
    Apply(ValidConfig),
    /// A resume, which validates the saved settings first.
    Validate,
}

struct Owners {
    runtime: Runtime,
    monitor: InputMonitor,
}

impl Owners {
    fn request_stop(&self) {
        self.runtime.request_stop();
        self.monitor.request_stop();
    }

    /// Asks both owners to stop, returning a future that resolves once both have exited.
    fn retire(&self) -> impl Future<Output = ()> + use<> {
        self.request_stop();
        let runtime = self.runtime.stopped();
        let monitor = self.monitor.stopped();
        async move {
            tokio::join!(runtime, monitor);
        }
    }
}

impl Drop for Owners {
    fn drop(&mut self) {
        // Each field's Drop asks only itself to stop before joining its threads; asking both first
        // lets their teardowns overlap.
        self.request_stop();
    }
}

pub(crate) struct Services {
    lifecycle: Lifecycle<Owners, Restart>,
    retirement: Option<Task<()>>,
    validation: Option<Task<()>>,
    pub(super) quit: Option<Task<()>>,
    pub(super) path: PathBuf,
    /// Whether `path` is the default settings file, which a launch without `--config` opens. Only
    /// those settings own Windows' sign-in launch, and a desktop shortcut need not name them.
    pub(super) default_settings: bool,
    pub config: Config,
    pub output: watch::Sender<Snapshot>,
    /// Where microphone levels reach the pill.
    pub(super) meter: watch::Sender<Meter>,
    /// Where a Show's live ink reaches the overlay.
    pub(super) canvas: watch::Sender<Canvas>,
    pub(super) pill: WindowHandle<Pill>,
    pub(super) settings: Option<WindowHandle<Settings>>,
    pub(super) shows: Option<WindowHandle<Review>>,
    pub mode: LaunchMode,
    pub(super) visibility: Option<Task<()>>,
    /// The menu's latest copy or opening of the last Show; a newer one replaces it.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(super) recovery: Option<Task<()>>,
    pub(super) tray_hint_seen: bool,
    requests: Option<Task<()>>,
    /// The latest microphone scan, which every Settings view shares and Quit waits for.
    pub(super) microphone_scan: Option<MicrophoneScan>,
    instance: Option<Instance>,
    pub(super) show_edits: crate::show::edits::Edits,
}

impl Global for Services {}

impl Services {
    #[expect(
        clippy::too_many_arguments,
        reason = "The composition root hands over each lane and handle Services owns; a parameter struct would only rename this list"
    )]
    pub(crate) fn new(
        path: PathBuf,
        default_settings: bool,
        config: Config,
        output: watch::Sender<Snapshot>,
        meter: watch::Sender<Meter>,
        canvas: watch::Sender<Canvas>,
        pill: WindowHandle<Pill>,
        instance: Option<Instance>,
        mode: LaunchMode,
    ) -> Self {
        Self {
            lifecycle: Lifecycle::default(),
            retirement: None,
            validation: None,
            quit: None,
            path,
            default_settings,
            config,
            output,
            meter,
            canvas,
            pill,
            settings: None,
            shows: None,
            mode,
            visibility: None,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            recovery: None,
            tray_hint_seen: false,
            requests: None,
            microphone_scan: None,
            instance,
            show_edits: crate::show::edits::Edits::default(),
        }
    }

    /// Releases every service, handing back the instance lock for the caller to drop last.
    pub(crate) fn into_instance(self) -> Option<Instance> {
        self.instance
    }

    /// Whether dictation is on: live owners run, or a resume is checking the saved settings.
    pub(crate) fn is_running(&self) -> bool {
        self.lifecycle
            .is_running(|owners| owners.runtime.is_running())
    }

    /// The epoch a save records when it is requested.
    pub(super) const fn config_epoch(&self) -> ConfigEpoch {
        self.lifecycle.epoch()
    }

    pub(super) fn is_quitting(&self) -> bool {
        self.lifecycle.is_quitting()
    }

    pub(crate) fn service_state(&self) -> ServiceState {
        if self.is_quitting() {
            ServiceState::Quitting
        } else if self.lifecycle.is_pausing() {
            ServiceState::Pausing
        } else if self.is_running() || self.mode.is_demo() {
            ServiceState::Running
        } else if self.config.needs_engine() {
            ServiceState::NotSetUp
        } else {
            ServiceState::Paused
        }
    }

    pub(crate) fn status(&self, snapshot: &Snapshot) -> Status {
        Status::new(snapshot, self.service_state(), self.config.engine)
    }

    fn runtime(&self) -> Option<&Runtime> {
        self.lifecycle.active().map(|owners| &owners.runtime)
    }

    /// Serves reveal, toggle, and cancel requests from later launches.
    pub(crate) fn listen_for_requests(
        &mut self,
        requests: async_channel::Receiver<Request>,
        cx: &App,
    ) {
        self.requests = Some(cx.spawn(async move |cx| {
            while let Ok(request) = requests.recv().await {
                if cx.update(|cx| handle_request(request, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// The Show that Copy last Show and Open last Show act on in `snapshot`, if they act: the one
    /// it remembers, while no session is under way, since a copy could race the session to the
    /// clipboard and its notice would not show. They need no dictation, so they act while paused,
    /// but neither in a demo nor once Quit begins.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn recallable_show<'a>(&self, snapshot: &'a Snapshot) -> Option<&'a SavedShow> {
        recallable(snapshot, self.service_state(), self.mode)
    }

    pub(super) fn recover_show(
        &self,
        interrupted: crate::show::Interrupted,
    ) -> Result<crate::runtime::RecoveryReceipt, &'static str> {
        if self.mode.is_demo() || self.is_quitting() {
            return Err("Recovery is unavailable in this preview or while quitting.");
        }
        self.runtime()
            .filter(|runtime| runtime.is_running())
            .ok_or("Resume Speakeasy to recover this Show.")?
            .recover(interrupted)
    }

    pub(super) fn can_copy_show(&self) -> bool {
        clipboard_free(&self.output.borrow(), self.service_state(), self.mode)
    }

    /// Shows `report`, such as a failure, unless a recording is under way, which it must not
    /// interrupt. A running owner keeps it until the next recording or settings change; otherwise
    /// the check and the write are one step, and no owner remains to publish over it.
    pub(crate) fn report(&self, report: impl Into<Report>) {
        let report = report.into();
        let unsent = match self.runtime().filter(|runtime| runtime.is_running()) {
            Some(runtime) => runtime.report(report),
            None => Err(report),
        };
        if let Err(report) = unsent {
            self.output
                .send_if_modified(|snapshot| report.present(snapshot));
        }
    }

    pub(crate) fn publish_error(&self, error: &anyhow::Error) {
        self.output.send_modify(|snapshot| {
            snapshot.phase = Phase::Error;
            snapshot.message = error.to_string();
        });
    }

    /// Applies `config`: reconfigures live owners, queues behind retiring ones, or starts new ones.
    /// Services adopts `config` even when new owners fail to start, and dictation then stays off.
    /// Does nothing once Quit has begun.
    pub(crate) fn apply(&mut self, config: ValidConfig, cx: &App) -> anyhow::Result<()> {
        if !self.lifecycle.reconfigure() {
            return Ok(());
        }
        // A dead runtime or changed desktop options need fresh owners: pausing makes the queue
        // below restart with `config` once the current owners retire.
        if self.runtime().is_some_and(|runtime| {
            !runtime.is_running() || self.config.change_to(&config) == Change::Desktop
        }) {
            self.pause(cx);
        }
        self.config = Config::clone(&config);
        if self.lifecycle.queue(Restart::Apply(config.clone())) {
            return Ok(());
        }
        match self.runtime() {
            Some(runtime) => {
                runtime.configure(config);
                Ok(())
            },
            None => self.start(config),
        }
    }

    /// Starts owners for `config` as [`Lifecycle::start`] allows, ending any resume's check. A
    /// failure leaves the status paused rather than loading.
    fn start(&mut self, config: ValidConfig) -> anyhow::Result<()> {
        let Self {
            lifecycle,
            output,
            meter,
            canvas,
            ..
        } = self;
        let started = lifecycle.start(|| {
            announce(output, runtime::LOADING);
            let (runtime, monitor) =
                Runtime::start(config, output.clone(), meter.clone(), canvas.clone())?;
            anyhow::Ok(Owners { runtime, monitor })
        });
        self.validation = None;
        if started.is_err() {
            announce(&self.output, PAUSED);
        }
        started.map(|_| ())
    }

    /// Adopts settings that a save made durable, enabling dictation only as
    /// [`Lifecycle::save_may_enable`] allows; otherwise the settings persist and show while
    /// dictation stays off. On Windows, every save of the default settings also registers Start at
    /// login as they say, so saving again retries a refused write, and its failure is returned
    /// unless adopting failed first.
    pub(super) fn adopt_saved(
        &mut self,
        config: ValidConfig,
        saved_at: ConfigEpoch,
        cx: &App,
    ) -> anyhow::Result<()> {
        #[cfg(target_os = "windows")]
        let start_at_login = config.start_at_login;
        let adopted = if self
            .lifecycle
            .save_may_enable(saved_at, |owners| owners.runtime.is_running())
        {
            self.apply(config, cx)
        } else {
            self.config = config.into_inner();
            Ok(())
        };
        #[cfg(target_os = "windows")]
        if self.default_settings {
            let registered = speakeasy_platform::start_at_login(start_at_login);
            return adopted.and(registered);
        }
        adopted
    }

    /// Checks the saved settings off the UI thread, then enables dictation with them, unless
    /// dictation is on, pausing, or quitting. A dead runtime retires first.
    pub(super) fn resume(&mut self, cx: &App) {
        if self.is_running() || self.lifecycle.is_pausing() {
            return;
        }
        if self.runtime().is_some() {
            self.pause(cx);
            self.lifecycle.queue(Restart::Validate);
            return;
        }
        let Some(epoch) = self.lifecycle.validate() else {
            return;
        };
        let config = self.config.clone();
        let path = self.path.clone();
        let validation = cx
            .background_executor()
            .spawn(async move { config.validated(&path) });
        announce(&self.output, CHECKING);
        self.validation = Some(cx.spawn(async move |cx| {
            let result = validation.await;
            cx.update_if_running(|cx| Self::validation_complete(epoch, result, cx));
        }));
    }

    fn validation_complete(epoch: ConfigEpoch, result: anyhow::Result<ValidConfig>, cx: &mut App) {
        let applied = cx.update_global::<Self, _>(|services, cx| {
            if !services.lifecycle.validated(epoch) {
                return Ok(());
            }
            services.validation = None;
            result.and_then(|config| services.apply(config, cx))
        });
        let Err(error) = applied else {
            return;
        };
        // Settings shows the reason; if it cannot open, the pill shows both.
        let error = match window::open(cx) {
            Ok(()) => error,
            Err(open_error) => anyhow!(
                "{}. Cannot open Settings: {open_error:#}",
                error.to_string().trim_end_matches('.')
            ),
        };
        cx.global::<Self>().publish_error(&error);
    }

    pub(super) fn pause(&mut self, cx: &App) {
        if self.is_quitting() {
            return;
        }
        self.validation = None;
        let started = self.lifecycle.pause();
        announce(
            &self.output,
            if self.lifecycle.is_pausing() {
                PAUSING
            } else {
                PAUSED
            },
        );
        if !started {
            return;
        }
        let Some(stopped) = self.lifecycle.retiring().map(Owners::retire) else {
            return;
        };
        self.retirement = Some(cx.spawn(async move |cx| {
            stopped.await;
            cx.update_if_running(|cx| {
                cx.update_global::<Self, _>(|services, cx| services.retired(cx));
            });
        }));
    }

    fn retired(&mut self, cx: &App) {
        let pending = self.lifecycle.retired();
        self.retirement = None;
        if self.is_quitting() {
            return;
        }
        match pending {
            Some(Restart::Apply(config)) => {
                if let Err(error) = self.apply(config, cx) {
                    self.publish_error(&error);
                }
            },
            Some(Restart::Validate) => self.resume(cx),
            None => announce(&self.output, PAUSED),
        }
    }

    pub(super) fn begin_quit(&mut self) -> Option<impl Future<Output = ()> + use<>> {
        if !self.lifecycle.quit() {
            return None;
        }
        self.validation = None;
        self.requests = None;
        self.visibility = None;
        // A copy or opening of the last Show still under way would report over the quit.
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            self.recovery = None;
        }
        announce(&self.output, QUITTING);
        let stopped = self.lifecycle.retiring().map(Owners::retire);
        let scanned = self.microphone_scan.as_ref().map(MicrophoneScan::exited);
        let edited = self.show_edits.drained();
        Some(async move {
            edited.await;
            if let Some(stopped) = stopped {
                stopped.await;
            }
            if let Some(scanned) = scanned {
                scanned.await;
            }
        })
    }

    pub(super) fn finish_quit(&mut self) {
        self.lifecycle.retired();
        self.retirement = None;
    }
}

pub(crate) fn toggle_enabled(cx: &mut App) {
    cx.update_global::<Services, _>(|services, cx| {
        if services.mode.is_demo() {
            return;
        }
        if services.is_running() {
            services.pause(cx);
        } else {
            services.resume(cx);
        }
    });
}

pub(crate) fn send(input: Input, cx: &App) {
    if let Some(runtime) = cx.global::<Services>().runtime() {
        runtime.input.deliver(input);
    }
}

/// Publishes a status of the shell's own that replaces the last owner's report.
fn announce(output: &watch::Sender<Snapshot>, message: &str) {
    output.send_modify(|snapshot| *snapshot = announcement(snapshot, message));
}

/// The snapshot after `previous` that says the shell's `message`, keeping only the last saved Show.
/// No owner has reported a model under it yet, so the model reads as loading rather than
/// unavailable.
fn announcement(previous: &Snapshot, message: &str) -> Snapshot {
    Snapshot {
        epoch: previous.epoch.next(),
        last_show: previous.last_show.clone(),
        message: message.into(),
        model: ModelState::Loading,
        ..Snapshot::default()
    }
}

/// The rule [`Services::recallable_show`] applies to `snapshot` under `service` and `mode`.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
fn recallable(snapshot: &Snapshot, service: ServiceState, mode: LaunchMode) -> Option<&SavedShow> {
    snapshot
        .last_show
        .as_ref()
        .filter(|_| clipboard_free(snapshot, service, mode))
}

fn clipboard_free(snapshot: &Snapshot, service: ServiceState, mode: LaunchMode) -> bool {
    !snapshot.phase.is_active() && service != ServiceState::Quitting && !mode.is_demo()
}

fn handle_request(request: Request, cx: &mut App) {
    let input = match request {
        Request::Reveal => {
            reveal(cx);
            return;
        },
        Request::Toggle => Input::Toggle,
        Request::Cancel => Input::Cancel,
    };
    // Only Linux desktop bindings drive dictation through a relaunch; otherwise the native shortcut
    // owns the gesture.
    if cfg!(target_os = "linux") && cx.global::<Services>().config.linux.external_shortcut {
        send(input, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Engine, status::Indicator};

    #[test]
    fn a_resume_check_and_a_start_read_as_busy_until_the_owner_reports() {
        for message in [CHECKING, runtime::LOADING] {
            let status = Status::new(
                &announcement(&Snapshot::default(), message),
                ServiceState::Running,
                Engine::Whisper,
            );
            assert_eq!(status.indicator, Indicator::Busy, "{}", status.description);
        }
    }

    #[test]
    fn an_announcement_replaces_the_status_but_keeps_the_last_show() {
        let show = SavedShow::assume("shows/20261029T120000Z");
        let previous = Snapshot {
            phase: Phase::Error,
            message: "Disk full".into(),
            last_show: Some(show.clone()),
            ..Snapshot::default()
        };
        let paused = announcement(&previous, PAUSED);
        assert_eq!(paused.epoch, previous.epoch.next());
        assert_eq!(
            (paused.phase, paused.message.as_str()),
            (Phase::Idle, PAUSED)
        );
        assert_eq!(paused.last_show, Some(show), "Pausing forgot the last Show");
    }

    #[test]
    fn the_last_show_is_recallable_only_between_sessions_outside_a_demo_and_quit() {
        use LaunchMode::{Demo, Live};
        use Phase::{Copied, Error, Idle, Processing, Recording};
        use ServiceState::{Paused, Pausing, Quitting, Running};
        let show = SavedShow::assume("shows/20261029T120000Z");
        // Remembered, service, phase, mode: whether Copy and Open last Show act.
        let table = [
            (true, Running, Copied, Live, true),
            (false, Running, Idle, Live, false),
            (true, Running, Recording, Live, false),
            (true, Running, Processing, Live, false),
            (true, Pausing, Idle, Live, true),
            (true, Paused, Error, Live, true),
            (true, Quitting, Idle, Live, false),
            (true, Running, Idle, Demo, false),
        ];
        for (remembered, service, phase, mode, expected) in table {
            let snapshot = Snapshot {
                phase,
                last_show: remembered.then(|| show.clone()),
                ..Snapshot::default()
            };
            assert_eq!(
                recallable(&snapshot, service, mode).is_some(),
                expected,
                "remembered {remembered}, {service:?}, {phase:?}, {mode:?}"
            );
        }
    }
}
