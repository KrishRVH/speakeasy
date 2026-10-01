//! UI-thread ownership of native services. Cleanup waits on acknowledgements, retaining
//! thread-affine adapters here until their native work has stopped.

use std::path::PathBuf;

use gpui::{App, BorrowAppContext, Global, Task, WindowHandle};
use speakeasy_platform::{Input, InputMonitor, ServiceState};
use tokio::sync::watch;

use super::{
    lifecycle::{ConfigEpoch, Lifecycle},
    settings::Settings,
    window::reveal,
};
use crate::{
    config::Config,
    gpui_ext::AppUpdate,
    instance::{Instance, Request},
    pill::Pill,
    runtime::{self, Phase, Runtime, Snapshot},
    status::Status,
};

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
    Apply(Config),
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

    fn stopped(&self) -> impl Future<Output = ()> + use<> {
        let runtime = self.runtime.stopped();
        let monitor = self.monitor.stopped();
        async move {
            tokio::join!(runtime, monitor);
        }
    }
}

impl Drop for Owners {
    fn drop(&mut self) {
        self.request_stop();
    }
}

pub(crate) struct Services {
    lifecycle: Lifecycle<Owners, Restart>,
    retirement: Option<Task<()>>,
    validation: Option<Task<()>>,
    pub(super) quit: Option<Task<()>>,
    pub(super) path: PathBuf,
    pub config: Config,
    pub(super) config_epoch: ConfigEpoch,
    pub output: watch::Sender<Snapshot>,
    pub(super) pill: WindowHandle<Pill>,
    pub(super) settings: Option<WindowHandle<Settings>>,
    pub mode: LaunchMode,
    pub(super) visibility: Option<Task<()>>,
    pub(super) tray_hint_seen: bool,
    requests: Option<Task<()>>,
    instance: Option<Instance>,
}

impl Global for Services {}

impl Services {
    pub(crate) fn new(
        path: PathBuf,
        config: Config,
        output: watch::Sender<Snapshot>,
        pill: WindowHandle<Pill>,
        instance: Option<Instance>,
        mode: LaunchMode,
    ) -> Self {
        Self {
            lifecycle: Lifecycle::Disabled,
            retirement: None,
            validation: None,
            quit: None,
            path,
            config,
            config_epoch: ConfigEpoch::default(),
            output,
            pill,
            settings: None,
            mode,
            visibility: None,
            tray_hint_seen: false,
            requests: None,
            instance,
        }
    }

    /// Releases every service, handing back the instance lock for the caller to drop last.
    pub(crate) fn into_instance(self) -> Option<Instance> {
        self.instance
    }

    pub(crate) fn running(&self) -> bool {
        self.runtime().is_some_and(Runtime::is_running)
            || self.lifecycle.validating(self.config_epoch)
    }

    pub(super) fn quitting(&self) -> bool {
        self.lifecycle.quitting()
    }

    pub(crate) fn service_state(&self) -> ServiceState {
        if self.lifecycle.pausing() {
            ServiceState::Pausing
        } else if self.running() || self.mode.is_demo() {
            ServiceState::Running
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

    pub(crate) fn publish_error(&self, error: &anyhow::Error) {
        self.output.send_modify(|snapshot| {
            snapshot.phase = Phase::Error;
            snapshot.message = error.to_string();
        });
    }

    fn announce(&self, message: &str) {
        self.output.send_modify(|snapshot| {
            *snapshot = Snapshot {
                epoch: snapshot.epoch.wrapping_add(1),
                message: message.into(),
                ..Snapshot::default()
            };
        });
    }

    /// Applies `config`: reconfigures live owners, queues behind retiring ones, or starts new ones.
    /// Does nothing once Quit has begun.
    pub(crate) fn apply(&mut self, config: Config, cx: &App) -> anyhow::Result<()> {
        if self.quitting() {
            return Ok(());
        }
        self.config_epoch = self.config_epoch.next();
        // A dead runtime or changed Linux desktop options need fresh owners: stopping makes the
        // queue below restart with `config` once the old owners retire.
        if self.runtime().is_some_and(|runtime| !runtime.is_running()) {
            self.stop(cx);
        }
        #[cfg(target_os = "linux")]
        if self.runtime().is_some() && self.config.linux != config.linux {
            self.stop(cx);
        }
        if self.lifecycle.queue(Restart::Apply(config.clone())) {
            self.config = config;
            return Ok(());
        }
        if let Some(runtime) = self.runtime() {
            runtime.configure(config.clone());
        } else {
            self.announce(runtime::LOADING);
            let (runtime, monitor) = Runtime::start(config.clone(), self.output.clone())?;
            self.lifecycle = Lifecycle::Running(Owners { runtime, monitor });
        }
        self.config = config;
        Ok(())
    }

    /// Adopts settings that a save made durable, enabling dictation only as `save_may_enable`
    /// allows; otherwise the settings persist and show while dictation stays off.
    pub(super) fn adopt_saved(
        &mut self,
        config: Config,
        saved_at: ConfigEpoch,
        cx: &App,
    ) -> anyhow::Result<()> {
        let enabled = self.running() || self.lifecycle.has_pending();
        if save_may_enable(saved_at, self.config_epoch, enabled) {
            self.apply(config, cx)
        } else {
            self.config = config;
            Ok(())
        }
    }

    fn resume(&mut self, cx: &App) {
        if self.lifecycle.pausing() {
            return;
        }
        if self.runtime().is_some() {
            self.stop(cx);
            self.lifecycle.queue(Restart::Validate);
            return;
        }
        self.config_epoch = self.config_epoch.next();
        let epoch = self.config_epoch;
        if !self.lifecycle.validate(epoch) {
            return;
        }
        let mut config = self.config.clone();
        let path = self.path.clone();
        let validation = cx.background_executor().spawn(async move {
            config.validate(&path)?;
            Ok::<_, anyhow::Error>(config)
        });
        self.announce("Checking local speech settings…");
        self.validation = Some(cx.spawn(async move |cx| {
            let result = validation.await;
            cx.update_if_running(|cx| Self::validation_complete(epoch, result, cx));
        }));
    }

    fn validation_complete(epoch: ConfigEpoch, result: anyhow::Result<Config>, cx: &mut App) {
        if !cx.has_global::<Self>() {
            return;
        }
        let result = cx.update_global::<Self, _>(|services, cx| {
            if !services.lifecycle.validating(epoch) {
                return Ok(());
            }
            services.lifecycle = Lifecycle::Disabled;
            services.validation = None;
            result.and_then(|config| services.apply(config, cx))
        });
        if let Err(error) = result {
            cx.global::<Self>().publish_error(&error);
            reveal(cx);
        }
    }

    pub(super) fn stop(&mut self, cx: &App) {
        if self.quitting() {
            return;
        }
        self.config_epoch = self.config_epoch.next();
        self.validation = None;
        let started = self.lifecycle.pause();
        self.announce(if self.lifecycle.pausing() {
            "Pausing dictation…"
        } else {
            "Dictation paused"
        });
        if !started {
            return;
        }
        let Some(owners) = self.lifecycle.retiring() else {
            return;
        };
        owners.request_stop();
        let stopped = owners.stopped();
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
        if self.quitting() {
            return;
        }
        match pending {
            Some(Restart::Apply(config)) => {
                if let Err(error) = self.apply(config, cx) {
                    self.publish_error(&error);
                }
            },
            Some(Restart::Validate) => self.resume(cx),
            None => self.announce("Dictation paused"),
        }
    }

    pub(super) fn begin_quit(&mut self) -> Option<impl Future<Output = ()> + use<>> {
        if !self.lifecycle.quit() {
            return None;
        }
        self.config_epoch = self.config_epoch.next();
        self.validation = None;
        self.requests = None;
        self.visibility = None;
        self.announce("Quitting Speakeasy…");
        let stopped = self.lifecycle.retiring().map(|owners| {
            owners.request_stop();
            owners.stopped()
        });
        Some(async move {
            if let Some(stopped) = stopped {
                stopped.await;
            }
        })
    }

    pub(super) fn finish_quit(&mut self) {
        self.lifecycle.retired();
        self.retirement = None;
    }
}

pub(crate) fn toggle_enabled(cx: &mut App) {
    if cx.global::<Services>().mode.is_demo() {
        return;
    }
    cx.update_global::<Services, _>(|services, cx| {
        if services.lifecycle.pausing() {
            return;
        }
        if services.running() {
            services.stop(cx);
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

/// A Pause after a Save wins: the save may enable dictation only if the service state is unchanged
/// since its request, or dictation is enabled anyway.
fn save_may_enable(saved_at: ConfigEpoch, current: ConfigEpoch, enabled: bool) -> bool {
    saved_at == current || enabled
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

    #[test]
    fn a_pause_after_save_keeps_dictation_off_unless_resumed() {
        let saved_at = ConfigEpoch::default();
        let paused_at = saved_at.next();
        assert!(save_may_enable(saved_at, saved_at, false));
        assert!(!save_may_enable(saved_at, paused_at, false));
        assert!(
            save_may_enable(saved_at, paused_at, true),
            "A deliberate Resume after Pause still receives the durable settings"
        );
    }
}
