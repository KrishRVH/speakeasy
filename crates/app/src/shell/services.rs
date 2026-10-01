//! UI-thread ownership of native services. Cleanup waits on acknowledgements,
//! retaining thread-affine adapters here until their native work has stopped.
use super::{lifecycle::Lifecycle, *};

enum Restart {
    Apply(Config),
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
    pub path: PathBuf,
    pub config: Config,
    pub configuration_epoch: u64,
    pub output: watch::Sender<Snapshot>,
    pub pill: WindowHandle<Pill>,
    pub window: Option<WindowHandle<Settings>>,
    pub demo: bool,
    pub demo_tray: bool,
    pub visibility: Option<Task<()>>,
    pub tray_hint_seen: bool,
    reopen: Option<Task<()>>,
    pub instance: Option<crate::instance::Instance>,
}
impl Global for Services {}
impl Services {
    pub(crate) fn new(
        path: PathBuf,
        config: Config,
        output: watch::Sender<Snapshot>,
        pill: WindowHandle<Pill>,
        instance: Option<crate::instance::Instance>,
    ) -> Self {
        Self {
            lifecycle: Lifecycle::Disabled,
            retirement: None,
            validation: None,
            quit: None,
            path,
            config,
            output,
            pill,
            window: None,
            demo: false,
            demo_tray: false,
            configuration_epoch: 0,
            visibility: None,
            tray_hint_seen: false,
            reopen: None,
            instance,
        }
    }
    pub(crate) fn running(&self) -> bool {
        self.runtime().is_some_and(Runtime::is_running)
            || self.lifecycle.validating(self.configuration_epoch)
    }
    pub(crate) fn runtime(&self) -> Option<&Runtime> {
        self.lifecycle.active().map(|owners| &owners.runtime)
    }
    pub(crate) fn pausing(&self) -> bool {
        self.lifecycle.pausing()
    }
    pub(crate) fn quitting(&self) -> bool {
        self.lifecycle.quitting()
    }
    pub(crate) fn has_pending(&self) -> bool {
        self.lifecycle.has_pending()
    }

    pub(crate) fn listen_for_reopen(
        &mut self,
        requests: async_channel::Receiver<crate::instance::Request>,
        cx: &App,
    ) {
        self.reopen = Some(cx.spawn(async move |cx| {
            while let Ok(request) = requests.recv().await {
                if cx.update(|cx| Self::instance_request(request, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn instance_request(request: crate::instance::Request, cx: &mut App) {
        use crate::instance::Request;

        match request {
            Request::Reveal => reveal(cx),
            Request::Toggle | Request::Cancel => {
                #[cfg(target_os = "linux")]
                if cx.global::<Services>().config.linux.external_shortcut {
                    send(
                        if request == Request::Toggle {
                            Input::Toggle
                        } else {
                            Input::Cancel
                        },
                        cx,
                    );
                }
            },
        }
    }

    fn notice(&self, message: &str) {
        self.output.send_modify(|snapshot| {
            *snapshot = Snapshot {
                epoch: snapshot.epoch.wrapping_add(1),
                message: message.into(),
                ..Snapshot::default()
            };
        });
    }
    pub(crate) fn apply(&mut self, config: Config, cx: &App) -> anyhow::Result<()> {
        if self.quitting() {
            return Ok(());
        }
        self.configuration_epoch = self.configuration_epoch.wrapping_add(1);
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
            self.notice(crate::runtime::LOADING);
            let (runtime, monitor) = Runtime::start(config.clone(), self.output.clone())?;
            self.lifecycle = Lifecycle::Running(Owners { runtime, monitor });
        }
        self.config = config;
        Ok(())
    }
    pub(super) fn resume(&mut self, cx: &App) {
        if self.pausing() {
            return;
        }
        if self.runtime().is_some() {
            self.stop(cx);
            self.lifecycle.queue(Restart::Validate);
            return;
        }
        self.configuration_epoch = self.configuration_epoch.wrapping_add(1);
        let epoch = self.configuration_epoch;
        if !self.lifecycle.validate(epoch) {
            return;
        }
        let mut config = self.config.clone();
        let path = self.path.clone();
        let validation = cx.background_executor().spawn(async move {
            config.validate(&path)?;
            Ok::<_, anyhow::Error>(config)
        });
        self.notice("Checking local speech settings…");
        self.validation = Some(cx.spawn(async move |cx| {
            let result = validation.await;
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Disposed UI services no longer need a validation result; they cannot restart dictation"
            )]
            let _ = cx.update(|cx| Self::validation_complete(epoch, result, cx));
        }));
    }

    fn validation_complete(epoch: u64, result: anyhow::Result<Config>, cx: &mut App) {
        if !cx.has_global::<Services>() {
            return;
        }
        let result = cx.update_global::<Services, _>(|services, cx| {
            if !services.lifecycle.validating(epoch) {
                return Ok(());
            }
            services.lifecycle = Lifecycle::Disabled;
            services.validation.take();
            result.and_then(|config| services.apply(config, cx))
        });
        if let Err(error) = result {
            cx.global::<Services>().output.send_modify(|snapshot| {
                snapshot.phase = Phase::Error;
                snapshot.message = error.to_string();
            });
            reveal(cx);
        }
    }

    pub(crate) fn stop(&mut self, cx: &App) {
        if self.quitting() {
            return;
        }
        self.configuration_epoch = self.configuration_epoch.wrapping_add(1);
        self.validation.take();
        let started = self.lifecycle.pause();
        self.notice(if self.pausing() {
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
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A disposed app needs no presentation update; native owners have acknowledged retirement"
            )]
            let _ = cx.update(|cx| {
                cx.update_global::<Services, _>(|services, cx| services.retired(cx));
            });
        }));
    }

    fn retired(&mut self, cx: &App) {
        let pending = self.lifecycle.retired();
        self.retirement.take();
        if self.quitting() {
            return;
        }
        match pending {
            Some(Restart::Apply(config)) => {
                if let Err(error) = self.apply(config, cx) {
                    self.output.send_modify(|snapshot| {
                        snapshot.phase = Phase::Error;
                        snapshot.message = error.to_string();
                    });
                }
            },
            Some(Restart::Validate) => self.resume(cx),
            None => self.notice("Dictation paused"),
        }
    }
    pub(super) fn begin_quit(&mut self) -> Option<impl Future<Output = ()> + use<>> {
        if !self.lifecycle.quit() {
            return None;
        }
        self.configuration_epoch = self.configuration_epoch.wrapping_add(1);
        self.validation.take();
        self.reopen.take();
        self.visibility.take();
        self.notice("Quitting Speakeasy…");
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
        self.retirement.take();
    }
}
