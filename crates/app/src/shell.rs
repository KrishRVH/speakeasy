use crate::{
    audio,
    config::{Config, Engine},
    pill::Pill,
    runtime::{ModelState, Phase, Runtime, Snapshot},
    setup::{self, Progress},
    status,
    theme::{Palette, alpha, mix},
};
use gpui::{prelude::*, *};
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
use raw_window_handle::HasWindowHandle;
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
use speakeasy_platform::Input;
use speakeasy_platform::InputMonitor;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::watch;

mod lifecycle;
mod services;
mod shutdown;
pub use services::Services;
pub use shutdown::request_quit;

pub fn reveal(cx: &mut App) {
    if !cx.has_global::<Services>() {
        return;
    }
    if let Err(error) = open(cx) {
        speakeasy_platform::show_error(&error.to_string());
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
pub fn toggle_enabled(cx: &mut App) {
    if cx.global::<Services>().demo {
        return;
    }
    cx.update_global::<Services, _>(|services, cx| {
        if services.pausing() {
            return;
        }
        if services.running() {
            services.stop(cx);
        } else {
            services.resume(cx);
        }
    });
}

// Resolve native handles immediately before use, outside GPUI's window borrow.
// Replacing the task cancels a queued hide if the app is reopened first.
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
fn settings_visible(handle: WindowHandle<Settings>, visible: bool, cx: &mut App) {
    let task = cx.spawn(async move |cx| {
        let raw = handle.update(cx, |_, window, _| {
            HasWindowHandle::window_handle(window).map(|handle| handle.as_raw())
        });
        if let Ok(Ok(raw)) = raw {
            speakeasy_platform::set_settings_visible(raw, visible);
            if visible {
                let _ = cx.update(|cx| cx.activate(true));
            } else {
                let _ = cx.update(|cx| {
                    let (pill, show_hint, path) = {
                        let services = cx.global::<Services>();
                        let path = services.path.with_file_name("tray-hint-seen");
                        let show = !services.tray_hint_seen && !path.exists();
                        (services.pill, show, path)
                    };
                    if show_hint {
                        let shown = pill
                            .update(cx, |pill, _, cx| pill.tray_hint(cx))
                            .unwrap_or(false);
                        if shown {
                            cx.global_mut::<Services>().tray_hint_seen = true;
                            let _ = std::fs::write(path, b"");
                        }
                    }
                });
            }
        }
    });
    cx.global_mut::<Services>().visibility = Some(task);
}

pub fn open(cx: &mut App) -> anyhow::Result<()> {
    if let Some(window) = cx.global::<Services>().window
        && window.update(cx, |_, _, _| ()).is_ok()
    {
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        settings_visible(window, true, cx);
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        let _ = window.update(cx, |_, window, _| window.activate_window());
        return Ok(());
    }
    let config = cx.global::<Services>().config.clone();
    let demo = cx.global::<Services>().demo;
    // A fresh install sets itself up; later launches leave engine choice alone.
    let first_run = !demo
        && cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        ))
        && !cx.global::<Services>().path.exists();
    let tray_lifecycle = !demo || cx.global::<Services>().demo_tray;
    let mut updates = cx.global::<Services>().output.subscribe();
    #[cfg(target_os = "windows")]
    let mut native_result = Ok(());
    let window = cx.open_window(
        WindowOptions {
            #[cfg(target_os = "linux")]
            app_id: Some(speakeasy_platform::APPLICATION_ID.into()),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(560.0), px(720.0)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Speakeasy".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| {
            #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
            let _ = (window, tray_lifecycle);
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            if tray_lifecycle {
                window.on_window_should_close(cx, |_, cx| {
                    if !cx.has_global::<Services>() {
                        return true;
                    }
                    #[cfg(target_os = "linux")]
                    if !cx.has_global::<crate::tray::Tray>()
                        || !cx.global::<crate::tray::Tray>().available
                    {
                        request_quit(cx);
                        return false;
                    }
                    if let Some(handle) = cx.global::<Services>().window {
                        settings_visible(handle, false, cx);
                    }
                    false
                });
            }
            #[cfg(target_os = "windows")]
            let minimize = if tray_lifecycle {
                let (hide, hidden) = async_channel::bounded(1);
                native_result = HasWindowHandle::window_handle(window)
                    .map_err(|error| anyhow::anyhow!("Cannot access Settings window: {error}"))
                    .and_then(|handle| speakeasy_platform::minimize_to_tray(handle.as_raw(), hide));
                Some(cx.spawn(async move |cx| {
                    while hidden.recv().await.is_ok() {
                        if cx
                            .update(|cx| {
                                if cx.has_global::<Services>()
                                    && let Some(handle) = cx.global::<Services>().window
                                {
                                    settings_visible(handle, false, cx);
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }))
            } else {
                None
            };
            cx.new(|cx: &mut Context<Settings>| {
                let task = cx.spawn(async move |this, cx| {
                    let mut previous = None;
                    loop {
                        let snapshot = updates.borrow_and_update().clone();
                        if this
                            .update(cx, |view, cx| {
                                let services = cx.global::<Services>();
                                let running = services.running() || services.demo;
                                let pausing = services.pausing();
                                let presentation = (
                                    status::indicator(&snapshot, running, pausing),
                                    snapshot.shortcut.clone(),
                                    snapshot.cancel_shortcut.clone(),
                                );
                                let status = if !snapshot.message.is_empty() {
                                    snapshot.message.clone()
                                } else {
                                    status::description(
                                        &snapshot,
                                        running,
                                        pausing,
                                        services.config.engine,
                                    )
                                };
                                if view.status != status || previous.as_ref() != Some(&presentation)
                                {
                                    view.status = status;
                                    previous = Some(presentation);
                                    cx.notify();
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                        if updates.changed().await.is_err() {
                            break;
                        }
                    }
                });
                let mut view = Settings {
                    config,
                    status: String::new(),
                    notice: None,
                    microphones: Vec::new(),
                    dialog: None,
                    devices: None,
                    preview: None,
                    setup: None,
                    retiring_setups: Vec::new(),
                    saving: None,
                    save_work: None,
                    saves: SaveQueue::default(),
                    progress: Progress::default(),
                    _updates: task,
                    demo,
                    #[cfg(target_os = "windows")]
                    _minimize: minimize,
                };
                if !demo {
                    view.refresh_devices(cx);
                }
                if first_run {
                    view.set_up(cx);
                }
                view
            })
        },
    )?;
    #[cfg(target_os = "windows")]
    if let Err(error) = native_result {
        let _ = window.update(cx, |_, window, _| window.remove_window());
        return Err(error);
    }
    cx.global_mut::<Services>().window = Some(window);
    if demo {
        let _ = window.update(cx, |view, _, cx| view.play(cx));
    }
    Ok(())
}

struct SaveRequest {
    draft: Config,
    epoch: u64,
    path: PathBuf,
}

struct SavedConfig {
    submitted: SaveRequest,
    validated: Config,
}

impl SavedConfig {
    fn update_draft(&self, draft: &mut Config) -> bool {
        if *draft == self.submitted.draft {
            *draft = self.validated.clone();
            true
        } else {
            false
        }
    }
}

fn save_may_enable(request_epoch: u64, current_epoch: u64, enabled: bool) -> bool {
    request_epoch == current_epoch || enabled
}

#[derive(Default)]
struct SaveQueue {
    pending: Option<SaveRequest>,
    saved: Option<SavedConfig>,
}

enum SaveProgress {
    Next(SaveRequest),
    Finished {
        saved: Option<Box<SavedConfig>>,
        error: Option<String>,
        epoch: u64,
    },
}

impl SaveQueue {
    fn finish(&mut self, submitted: SaveRequest, result: anyhow::Result<Config>) -> SaveProgress {
        let epoch = submitted.epoch;
        let error = match result {
            Ok(validated) => {
                self.saved = Some(SavedConfig {
                    submitted,
                    validated,
                });
                None
            }
            Err(error) => Some(error.to_string()),
        };
        if let Some(next) = self.pending.take() {
            SaveProgress::Next(next)
        } else {
            SaveProgress::Finished {
                saved: self.saved.take().map(Box::new),
                error,
                epoch,
            }
        }
    }
}

// One short-lived worker owns each durable write. Settings retains it until
// completion, or joins it at Quit before flushing the latest queued request.
struct SaveWork {
    result: async_channel::Receiver<anyhow::Result<Config>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SaveWork {
    fn start(mut config: Config, path: PathBuf) -> anyhow::Result<Self> {
        Self::spawn(move || {
            config.validate(&path)?;
            config.save(&path)?;
            Ok(config)
        })
    }

    fn spawn(
        write: impl FnOnce() -> anyhow::Result<Config> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (complete, result) = async_channel::bounded(1);
        let thread = std::thread::Builder::new()
            .name("settings-save".into())
            .spawn(move || {
                let _ = complete.try_send(write());
            })?;
        Ok(Self {
            result,
            thread: Some(thread),
        })
    }
}

impl Drop for SaveWork {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct Settings {
    config: Config,
    status: String,
    notice: Option<String>,
    microphones: Vec<(String, String)>,
    dialog: Option<Task<()>>,
    devices: Option<Task<()>>,
    preview: Option<Task<()>>,
    setup: Option<(setup::Setup, Task<()>)>,
    retiring_setups: Vec<(setup::Setup, Task<()>)>,
    saving: Option<Task<()>>,
    save_work: Option<SaveWork>,
    saves: SaveQueue,
    progress: Progress,
    _updates: Task<()>,
    demo: bool,
    #[cfg(target_os = "windows")]
    _minimize: Option<Task<()>>,
}

impl Drop for Settings {
    fn drop(&mut self) {
        if let Some((setup, _)) = &self.setup {
            setup.request_stop();
        }
        for (setup, _) in &self.retiring_setups {
            setup.request_stop();
        }
        // Quit may release the view before its foreground task observes the
        // completion. Keep filesystem writes ordered and own the latest Save.
        drop(self.save_work.take());
        if let Some(pending) = self.saves.pending.take()
            && let Ok(work) = SaveWork::start(pending.draft, pending.path)
        {
            drop(work);
        }
        drop(self.setup.take());
        self.retiring_setups.clear();
    }
}
#[derive(Clone, Copy)]
enum Action {
    Setup,
    Engine,
    Executable,
    Model,
    Microphone,
    Refresh,
    Language,
    Gpu,
    Clipboard,
    Motion,
    Theme,
    Save,
    Pause,
    Preview,
    Quit,
    LinuxManual,
    LinuxTerminal,
    LinuxExternal,
}

impl Settings {
    fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        let enumeration = audio::microphones();
        self.devices = Some(cx.spawn(async move |this, cx| {
            let result = enumeration.await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(devices) => view.microphones = devices,
                    Err(_) => {
                        view.notice =
                            Some("Cannot list microphones. Check OS audio settings.".into())
                    }
                }
                view.devices = None;
                cx.notify();
            });
        }));
    }
    /// Starts automatic setup, or cancels it while running.
    fn set_up(&mut self, cx: &mut Context<Self>) {
        if cx.global::<Services>().quitting() {
            return;
        }
        if let Some((setup, updates)) = self.setup.take() {
            drop(updates);
            setup.request_stop();
            let stopped = setup.stopped();
            let identity = setup.result.clone();
            let retirement = cx.spawn(async move |this, cx| {
                stopped.await;
                let _ = this.update(cx, |view, _| {
                    view.retiring_setups
                        .retain(|(setup, _)| !setup.result.same_channel(&identity));
                });
            });
            self.retiring_setups.push((setup, retirement));
            self.notice = Some("Setup paused. Downloads resume where they stopped.".into());
            return;
        }
        let setup = match setup::Setup::start() {
            Ok(setup) => setup,
            Err(error) => {
                self.notice = Some(error.to_string());
                return;
            }
        };
        let mut progress = setup.progress.clone();
        let result = setup.result.clone();
        let stopped = setup.stopped();
        self.notice = None;
        let task = cx.spawn(async move |this, cx| {
            while progress.changed().await.is_ok() {
                let current = progress.borrow_and_update().clone();
                if this
                    .update(cx, |view, cx| {
                        view.progress = current;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                Timer::after(Duration::from_millis(100)).await;
            }
            let result = result.recv().await;
            stopped.await;
            let _ = this.update(cx, |view, cx| {
                view.setup = None;
                view.progress = Progress::default();
                match result {
                    Ok(Ok(installed)) => {
                        view.config.engine = installed.engine;
                        view.config.engine_executable = installed.engine_executable;
                        view.config.model = installed.model;
                        view.config.use_gpu = installed.use_gpu;
                        view.save(cx);
                    }
                    Ok(Err(error)) => view.notice = Some(error.to_string()),
                    Err(_) => view.notice = Some("Setup stopped unexpectedly. Try again.".into()),
                }
                cx.notify();
            });
        });
        self.setup = Some((setup, task));
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        if cx.global::<Services>().quitting() {
            return;
        }
        let services = cx.global::<Services>();
        let epoch = services.configuration_epoch;
        let path = services.path.clone();
        self.saves.pending = Some(SaveRequest {
            draft: self.config.clone(),
            epoch,
            path,
        });
        self.notice = Some("Saving changes…".into());
        if self.saving.is_some() {
            cx.notify();
            return;
        }
        let Some(request) = self.saves.pending.take() else {
            return;
        };
        let Some((mut request, mut result)) = self.advance_save(SaveProgress::Next(request), cx)
        else {
            cx.notify();
            return;
        };
        self.saving = Some(cx.spawn(async move |this, cx| {
            loop {
                let completed = result.recv().await.unwrap_or_else(|_| {
                    Err(anyhow::anyhow!(
                        "Saving settings stopped unexpectedly. Try saving again."
                    ))
                });
                let next = this.update(cx, |view, cx| {
                    // The acknowledgement follows the durable write. Joining
                    // here cannot wait for storage or native work.
                    drop(view.save_work.take());
                    let progress = view.saves.finish(request, completed);
                    let next = view.advance_save(progress, cx);
                    cx.notify();
                    next
                });
                match next {
                    Ok(Some((next, incoming))) => {
                        request = next;
                        result = incoming;
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        }));
        cx.notify();
    }

    fn advance_save(
        &mut self,
        mut progress: SaveProgress,
        cx: &mut Context<Self>,
    ) -> Option<(SaveRequest, async_channel::Receiver<anyhow::Result<Config>>)> {
        loop {
            match progress {
                SaveProgress::Next(next) => {
                    match SaveWork::start(next.draft.clone(), next.path.clone()) {
                        Ok(work) => {
                            let result = work.result.clone();
                            self.save_work = Some(work);
                            return Some((next, result));
                        }
                        Err(error) => progress = self.saves.finish(next, Err(error)),
                    }
                }
                SaveProgress::Finished {
                    saved,
                    error,
                    epoch,
                } => {
                    self.finish_save(saved, error, epoch, cx);
                    self.saving = None;
                    return None;
                }
            }
        }
    }

    fn finish_save(
        &mut self,
        saved: Option<Box<SavedConfig>>,
        error: Option<String>,
        epoch: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(saved) = saved else {
            self.notice = error;
            return;
        };
        let unchanged = saved.update_draft(&mut self.config);
        let result = cx.update_global::<Services, _>(|services, cx| {
            if save_may_enable(
                epoch,
                services.configuration_epoch,
                services.running() || services.has_pending(),
            ) {
                services.apply(saved.validated, cx)
            } else {
                // Pause after Save wins over its delayed completion. Appearance
                // and persisted settings still update without resuming dictation.
                services.config = saved.validated;
                Ok(())
            }
        });
        self.notice = error
            .or_else(|| result.err().map(|error| error.to_string()))
            .or_else(|| (!unchanged).then(|| "Unsaved changes".into()));
        let pill = cx.global::<Services>().pill;
        let reduced =
            cx.global::<Services>().config.reduced_motion || speakeasy_platform::reduced_motion();
        let _ = pill.update(cx, |view, _, cx| {
            view.set_reduced(reduced);
            cx.notify();
        });
        show_theme(cx.global::<Services>().config.theme, cx);
    }
    fn choose(&mut self, model: bool, window: &Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let (title, filter) = match (self.config.engine, model) {
            (Engine::Parakeet, true) => (
                "Choose a Parakeet v3 GGUF model",
                ["Parakeet models", "*.gguf"],
            ),
            (Engine::Parakeet, false) => ("Choose nemo-speech", ["Programs", "*.exe"]),
            (Engine::Whisper, true) => ("Choose a Whisper GGML model", ["Whisper models", "*.bin"]),
            (Engine::Whisper, false) => ("Choose whisper-server", ["Programs", "*.exe"]),
        };
        #[cfg(not(target_os = "windows"))]
        let filter = if model { filter } else { ["Programs", "*"] };
        let picker = choose_file(window, title, filter, cx);
        self.dialog = Some(cx.spawn(async move |this, cx| {
            let result = picker.await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(Some(path)) => {
                        if model {
                            view.config.model = path;
                        } else {
                            view.config.engine_executable = path;
                        }
                        view.notice = Some("Unsaved changes".into());
                    }
                    Ok(None) => {}
                    Err(_) => {
                        view.notice = Some("Could not open the file picker. Try again.".into())
                    }
                }
                view.dialog = None;
                cx.notify();
            });
        }));
    }
    fn act(&mut self, action: Action, window: &Window, cx: &mut Context<Self>) {
        if cx.global::<Services>().quitting() {
            return;
        }
        match action {
            Action::Setup => self.set_up(cx),
            Action::Engine => {
                self.config.engine = match self.config.engine {
                    Engine::Whisper => Engine::Parakeet,
                    Engine::Parakeet => Engine::Whisper,
                };
                self.notice =
                    Some("Choose the executable and model for this engine, then save.".into());
            }
            Action::Executable => self.choose(false, window, cx),
            Action::Model => self.choose(true, window, cx),
            Action::Refresh => {
                if !self.demo {
                    self.refresh_devices(cx);
                }
            }
            Action::Microphone => {
                self.config.microphone = match &self.config.microphone {
                    None => self.microphones.first().map(|(id, _)| id.clone()),
                    Some(current) => self
                        .microphones
                        .iter()
                        .position(|(id, _)| id == current)
                        .and_then(|index| self.microphones.get(index + 1))
                        .map(|(id, _)| id.clone()),
                };
                self.notice = Some("Unsaved changes".into());
            }
            Action::Language => {
                if self.config.engine == Engine::Parakeet {
                    return;
                }
                self.config.language = if self.config.language == "en" {
                    "auto"
                } else {
                    "en"
                }
                .into();
                self.notice = Some("Unsaved changes".into());
            }
            Action::Gpu => {
                self.config.use_gpu = !self.config.use_gpu;
                self.notice = Some("Unsaved changes".into());
            }
            Action::Clipboard => {
                self.config.preserve_clipboard = !self.config.preserve_clipboard;
                self.notice = Some("Unsaved changes".into());
            }
            Action::LinuxManual => {
                self.config.linux.manual_paste = !self.config.linux.manual_paste;
                self.notice = Some("Unsaved changes".into());
            }
            Action::LinuxTerminal => {
                self.config.linux.terminal_paste = !self.config.linux.terminal_paste;
                self.notice = Some("Unsaved changes".into());
            }
            Action::LinuxExternal => {
                self.config.linux.external_shortcut = !self.config.linux.external_shortcut;
                self.notice = Some("Unsaved changes".into());
            }
            Action::Motion => {
                self.config.reduced_motion = !self.config.reduced_motion;
                self.notice = Some("Unsaved changes".into());
                if self.demo {
                    let pill = cx.global::<Services>().pill;
                    let reduced =
                        self.config.reduced_motion || speakeasy_platform::reduced_motion();
                    let _ = pill.update(cx, |view, _, cx| {
                        view.set_reduced(reduced);
                        cx.notify();
                    });
                    self.notice = None;
                }
            }
            Action::Theme => {
                self.config.theme = self.config.theme.next();
                self.notice = Some("Unsaved changes".into());
                if self.demo {
                    let theme = self.config.theme;
                    cx.update_global::<Services, _>(|services, _| services.config.theme = theme);
                    show_theme(theme, cx);
                    self.notice = None;
                }
            }
            Action::Save => {
                if self.demo {
                    self.notice =
                        Some("Preview only. Run without --demo to enable dictation.".into());
                } else {
                    self.save(cx);
                }
            }
            Action::Pause => {
                cx.update_global::<Services, _>(|services, cx| services.stop(cx));
                self.notice = None;
            }
            Action::Preview => self.play(cx),
            Action::Quit => request_quit(cx),
        }
        cx.notify();
    }
    fn setup_card(&self, palette: &'static Palette, cx: &Context<Self>) -> impl IntoElement {
        let running = self.setup.is_some();
        let Progress { step, done, total } = self.progress;
        let detail = if !running {
            format!(
                "Speakeasy downloads the Parakeet speech model and the engine that suits this {}, about 0.8 GB, then turns dictation on.",
                if cfg!(target_os = "macos") {
                    "Mac"
                } else {
                    "PC"
                }
            )
        } else if total > 0 {
            format!("{step} · {} of {} MB", done / 1_000_000, total / 1_000_000)
        } else if step.is_empty() {
            "Checking this machine…".into()
        } else {
            format!("{step}…")
        };
        panel(palette)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().text_size(px(13.0)).child(if running {
                "Setting up dictation"
            } else {
                "Set up dictation"
            }))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .child(detail),
            )
            .when(running, |card| {
                card.child(
                    div()
                        .h(px(3.0))
                        .rounded(px(2.0))
                        .bg(alpha(palette.lamp, 0.16))
                        .child(
                            div()
                                .h_full()
                                .rounded(px(2.0))
                                .bg(rgb(palette.lamp))
                                .w(relative(done as f32 / total.max(1) as f32)),
                        ),
                )
            })
            .child(div().flex().child(button(
                if running {
                    "Cancel"
                } else {
                    "Set up automatically"
                },
                Action::Setup,
                palette,
                cx,
            )))
    }
    fn play(&mut self, cx: &mut Context<Self>) {
        let tx = cx.global::<Services>().output.clone();
        let id = tx.borrow().id.wrapping_add(1);
        self.preview = Some(cx.spawn(async move |_, _| {
            let mut snapshot = Snapshot {
                id,
                phase: Phase::Starting,
                model: ModelState::Ready,
                ..Snapshot::default()
            };
            tx.send_replace(snapshot.clone());
            Timer::after(Duration::from_millis(350)).await;
            snapshot.phase = Phase::Recording;
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                let t = start.elapsed().as_secs_f32();
                snapshot.hands_free = t > 2.0;
                snapshot.meter_tick += 1;
                snapshot.level = ((t * 7.0).sin() * 0.45 + 0.35).max(0.0) * ((t * 2.1).sin().abs());
                tx.send_replace(snapshot.clone());
                Timer::after(Duration::from_millis(32)).await;
            }
            snapshot.started = Instant::now() - Duration::from_secs(272);
            snapshot.level = 0.0;
            snapshot.meter_tick += 1;
            tx.send_replace(snapshot.clone());
            Timer::after(Duration::from_secs(2)).await;
            snapshot.phase = Phase::Stopping;
            tx.send_replace(snapshot.clone());
            Timer::after(Duration::from_millis(150)).await;
            snapshot.phase = Phase::Processing;
            tx.send_replace(snapshot.clone());
            Timer::after(Duration::from_millis(1400)).await;
            snapshot.phase = Phase::Done;
            tx.send_replace(snapshot.clone());
            Timer::after(Duration::from_millis(450)).await;
            // Interrupt the outgoing completion, then cancel and immediately
            // begin a new recording. No demo callbacks touch the real runtime.
            for end in [Phase::Cancelled, Phase::Empty, Phase::Error] {
                snapshot = Snapshot {
                    id: snapshot.id.wrapping_add(1),
                    phase: Phase::Starting,
                    model: ModelState::Ready,
                    ..Snapshot::default()
                };
                tx.send_replace(snapshot.clone());
                Timer::after(Duration::from_millis(180)).await;
                snapshot.phase = Phase::Recording;
                tx.send_replace(snapshot.clone());
                Timer::after(Duration::from_millis(700)).await;
                snapshot.phase = end;
                if end == Phase::Error {
                    snapshot.message =
                        "Microphone disconnected. Choose an available microphone in Settings."
                            .into();
                }
                tx.send_replace(snapshot.clone());
                Timer::after(if end == Phase::Cancelled {
                    Duration::from_millis(60)
                } else {
                    Duration::from_secs(2)
                })
                .await;
            }
        }));
    }
}
// The pill and tray follow the saved theme; Settings previews unsaved edits.
fn show_theme(theme: crate::theme::Theme, cx: &mut App) {
    let pill = cx.global::<Services>().pill;
    let _ = pill.update(cx, |view, _, cx| {
        view.set_theme(theme);
        cx.notify();
    });
    // Republish the current snapshot so the tray redraws its icon.
    cx.global::<Services>().output.send_modify(|_| ());
}
fn button(
    label: impl Into<SharedString>,
    action: Action,
    palette: &'static Palette,
    cx: &Context<Settings>,
) -> impl IntoElement {
    let label = label.into();
    div()
        .id(label.clone())
        .focusable()
        .tab_stop(true)
        .px(px(12.0))
        .max_w(px(390.0))
        .overflow_hidden()
        .text_ellipsis()
        .py(px(8.0))
        .rounded(px(3.0))
        .border_1()
        .border_color(rgb(mix(palette.raise, palette.ink, 0.14)))
        .bg(rgb(palette.raise))
        .text_size(px(12.0))
        .hover(|s| s.bg(rgb(mix(palette.raise, palette.ink, 0.08))))
        .focus(|s| s.border_color(rgb(palette.lamp)))
        .cursor_pointer()
        .on_click(cx.listener(move |view, _, window, cx| view.act(action, window, cx)))
        .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                view.act(action, window, cx);
                cx.stop_propagation();
            }
        }))
        .child(label)
}
/// Windows gets the platform dialog: GPUI shows its picker inside the UI
/// thread's message loop, where it stays unpainted while GPUI is idle.
#[cfg(target_os = "windows")]
fn choose_file(
    window: &Window,
    title: &'static str,
    filter: [&'static str; 2],
    _: &App,
) -> impl Future<Output = anyhow::Result<Option<PathBuf>>> + use<> {
    let owner = HasWindowHandle::window_handle(window)
        .map(|handle| handle.as_raw())
        .map_err(|error| anyhow::anyhow!("Cannot access Settings window: {error}"));
    async move { speakeasy_platform::choose_file(owner?, title, filter).await }
}
#[cfg(not(target_os = "windows"))]
fn choose_file(
    _: &Window,
    title: &'static str,
    _: [&'static str; 2],
    cx: &App,
) -> impl Future<Output = anyhow::Result<Option<PathBuf>>> + use<> {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(title.into()),
    });
    async move { Ok(paths.await??.and_then(|paths| paths.into_iter().next())) }
}
fn panel(palette: &'static Palette) -> Div {
    div()
        .p(px(16.0))
        .rounded(px(4.0))
        .bg(rgb(palette.panel))
        .border_1()
        .border_color(alpha(palette.lamp, 0.14))
}
fn label(text: &'static str, palette: &'static Palette) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .text_color(rgb(palette.muted))
        .child(text)
}
// A cut-jewel status mark, matching the tray badges.
fn jewel(color: u32, size: f32) -> impl IntoElement {
    svg()
        .path(crate::icons::KEYSTONE)
        .size(px(size))
        .text_color(rgb(color))
        .flex_none()
}
// GPUI text has no tracking, so the wordmark spaces its capitals as glyphs.
fn wordmark(palette: &'static Palette) -> impl IntoElement {
    div()
        .flex()
        .gap(px(5.0))
        .font_family(crate::WORDMARK_FONT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(19.0))
        .text_color(rgb(palette.ink))
        .children(
            "SPEAKEASY"
                .chars()
                .map(|letter| div().child(letter.to_string())),
        )
}
fn keystone_rule(palette: &'static Palette) -> impl IntoElement {
    let rule = || div().flex_1().h(px(1.0)).bg(alpha(palette.lamp, 0.3));
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(rule())
        .child(jewel(palette.lamp, 6.0))
        .child(rule())
}
fn filename(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || "Not selected".into(),
        |name| name.to_string_lossy().into_owned(),
    )
}
impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (indicator, shortcut, cancel_shortcut) = {
            let services = cx.global::<Services>();
            let snapshot = services.output.borrow();
            (
                status::indicator(
                    &snapshot,
                    services.running() || services.demo,
                    services.pausing(),
                ),
                snapshot.shortcut.clone(),
                snapshot.cancel_shortcut.clone(),
            )
        };
        let desktop_bindings = cfg!(target_os = "linux") && self.config.linux.external_shortcut;
        let shortcut_hint = if desktop_bindings {
            "Use your desktop shortcut to start or finish dictation.".into()
        } else {
            format!("Hold {shortcut} to speak.")
        };
        let gesture_hint = if desktop_bindings {
            "Use your desktop's cancel binding to discard a recording.".into()
        } else if cfg!(target_os = "linux") {
            format!("Double-tap for hands-free. {cancel_shortcut} cancels.")
        } else {
            "Add Space, or double-tap, for hands-free. Escape cancels.".into()
        };
        let palette = self.config.theme.palette();
        let dim = mix(palette.room, palette.muted, 0.6);
        let indicator_color = match indicator {
            status::Indicator::Recording => palette.live,
            status::Indicator::Attention => palette.warn,
            status::Indicator::Ready => palette.lamp,
            status::Indicator::Busy | status::Indicator::Paused => dim,
        };
        let microphone = match &self.config.microphone {
            None => "System default".to_owned(),
            Some(id) => self
                .microphones
                .iter()
                .find(|(key, _)| key == id)
                .map_or_else(
                    || "Unavailable · choose another".into(),
                    |(_, name)| name.clone(),
                ),
        };
        let running = cx.global::<Services>().running();
        // Setup chooses the engine, so its manual controls wait until it ends.
        let installing = self.setup.is_some();
        div()
            .id("settings")
            .on_key_down(|event, window, cx| {
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                    cx.stop_propagation();
                }
            })
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(palette.room))
            .text_color(rgb(palette.ink))
            .p(px(32.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(jewel(indicator_color, 10.0))
                            .child(wordmark(palette)),
                    )
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(rgb(palette.muted))
                            .child("Say the word."),
                    )
                    .child(keystone_rule(palette)),
            )
            .child(
                if installing || !self.demo && self.config.engine_executable.as_os_str().is_empty()
                {
                    self.setup_card(palette, cx).into_any_element()
                } else {
                    panel(palette)
                        .text_size(px(13.0))
                        .line_height(px(23.0))
                        .child(shortcut_hint)
                        .child(div().child(gesture_hint))
                        .into_any_element()
                },
            )
            .when(!installing, |settings| {
                settings.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(label("LOCAL SPEECH", palette))
                                .child(button(
                                    match self.config.engine {
                                        Engine::Whisper => "Engine: Whisper",
                                        Engine::Parakeet => "Engine: Parakeet",
                                    },
                                    Action::Engine,
                                    palette,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(12.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_size(px(12.0))
                                        .child(filename(&self.config.engine_executable)),
                                )
                                .child(button(
                                    "Choose executable",
                                    Action::Executable,
                                    palette,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(12.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_size(px(12.0))
                                        .child(filename(&self.config.model)),
                                )
                                .child(button("Choose model", Action::Model, palette, cx)),
                        ),
                )
            })
            .when(cfg!(target_os = "linux"), |form| {
                form.child(
                    panel(palette)
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(label("LINUX DESKTOP", palette))
                        .child(div().text_size(px(12.0)).child(format!("Shortcut: {shortcut}")))
                        .child(div().text_size(px(12.0)).child(format!("Cancel: {cancel_shortcut}")))
                        .child(button(
                            if self.config.linux.external_shortcut {
                                "Shortcuts: Desktop bindings"
                            } else {
                                "Shortcuts: Native desktop"
                            },
                            Action::LinuxExternal,
                            palette,
                            cx,
                        ))
                        .child(div().text_size(px(12.0)).child(
                            "Desktop bindings: assign speakeasy --toggle and speakeasy --cancel in your desktop's shortcut settings.",
                        ))
                        .child(button(
                            if self.config.linux.manual_paste {
                                "Copy for manual paste: On"
                            } else {
                                "Copy for manual paste: Off"
                            },
                            Action::LinuxManual,
                            palette,
                            cx,
                        ))
                        .child(button(
                            if self.config.linux.terminal_paste {
                                "Paste shortcut: Ctrl+Shift+V"
                            } else {
                                "Paste shortcut: Ctrl+V"
                            },
                            Action::LinuxTerminal,
                            palette,
                            cx,
                        )),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(label("MICROPHONE", palette))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(button(microphone, Action::Microphone, palette, cx))
                            .child(button("Refresh", Action::Refresh, palette, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(label("PREFERENCES", palette))
                    .when(!installing, |section| {
                        section.child(
                            div()
                                .flex()
                                .gap(px(8.0))
                                .child(if self.config.engine == Engine::Parakeet {
                                    div()
                                        .py(px(8.0))
                                        .text_size(px(12.0))
                                        .child("Language: Automatic")
                                        .into_any_element()
                                } else {
                                    button(
                                        match self.config.language.as_str() {
                                            "auto" => "Language: Auto".to_owned(),
                                            "en" => "Language: English".to_owned(),
                                            language => format!("Language: {language}"),
                                        },
                                        Action::Language,
                                        palette,
                                        cx,
                                    )
                                    .into_any_element()
                                })
                                .child(button(
                                    if self.config.use_gpu {
                                        "Prefer GPU: On"
                                    } else {
                                        "Prefer GPU: Off"
                                    },
                                    Action::Gpu,
                                    palette,
                                    cx,
                                )),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(button(
                                if self.config.preserve_clipboard {
                                    "Keep clipboard: On"
                                } else {
                                    "Keep clipboard: Off"
                                },
                                Action::Clipboard,
                                palette,
                                cx,
                            ))
                            .child(button(
                                if self.config.reduced_motion {
                                    "Reduce motion: On"
                                } else {
                                    "Reduce motion: Off"
                                },
                                Action::Motion,
                                palette,
                                cx,
                            )),
                    )
                    .child(div().flex().child(button(
                        format!("Theme: {}", self.config.theme.name()),
                        Action::Theme,
                        palette,
                        cx,
                    ))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .when(!installing, |row| {
                        row.child(button(
                            if self.demo {
                                "Replay preview"
                            } else if running {
                                "Save changes"
                            } else {
                                "Enable dictation"
                            },
                            if self.demo {
                                Action::Preview
                            } else {
                                Action::Save
                            },
                            palette,
                            cx,
                        ))
                    })
                    .when(running, |row| {
                        row.child(button("Pause", Action::Pause, palette, cx))
                    })
                    .child(button("Quit", Action::Quit, palette, cx)),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(if self.notice.is_some() {
                        palette.warn
                    } else {
                        palette.muted
                    }))
                    .child(self.status.clone())
                    .when_some(self.notice.clone(), |status, notice| {
                        status.child(div().child(notice))
                    }),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(dim))
                    .child(if self.demo {
                        "Preview only · Simulated audio · No insertion"
                    } else {
                        "On your device · No account · No history"
                    }),
            )
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
pub fn send(action: Input, cx: &App) {
    if let Some(runtime) = cx.global::<Services>().runtime() {
        speakeasy_platform::deliver(&runtime.input, action);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Config, SaveProgress, SaveQueue, SaveRequest, SaveWork, SavedConfig, Settings,
        save_may_enable,
    };
    use std::{path::PathBuf, time::Duration};

    fn request(threads: u16, epoch: u64) -> SaveRequest {
        SaveRequest {
            draft: Config {
                threads,
                ..Config::default()
            },
            epoch,
            path: PathBuf::from("settings.json"),
        }
    }

    #[test]
    fn queued_save_applies_only_latest_successful_draft() -> anyhow::Result<()> {
        let mut queue = SaveQueue::default();
        let first = request(4, 0);
        let first_saved = first.draft.clone();
        queue.pending = Some(request(8, 0));
        // Repeated Save replaces the queued draft without overlapping writes.
        queue.pending = Some(request(16, 0));
        let SaveProgress::Next(next) = queue.finish(first, Ok(first_saved)) else {
            anyhow::bail!("Queued save was lost");
        };
        assert_eq!(next.draft.threads, 16);
        let saved = next.draft.clone();
        let SaveProgress::Finished {
            saved,
            error,
            epoch,
        } = queue.finish(next, Ok(saved))
        else {
            anyhow::bail!("Latest save did not finish");
        };
        let saved = saved.ok_or_else(|| anyhow::anyhow!("Successful save was lost"))?;
        assert_eq!(saved.validated.threads, 16);
        assert_eq!(epoch, 0);
        assert!(error.is_none());
        assert!(queue.pending.is_none());
        assert!(queue.saved.is_none());
        Ok(())
    }

    #[test]
    fn queued_failure_retains_previous_durable_save_and_reports_latest_error() -> anyhow::Result<()>
    {
        let mut queue = SaveQueue::default();
        let first = request(4, 0);
        let first_saved = first.draft.clone();
        queue.pending = Some(request(8, 1));
        let SaveProgress::Next(next) = queue.finish(first, Ok(first_saved)) else {
            anyhow::bail!("Queued save was lost");
        };
        let SaveProgress::Finished {
            saved,
            error,
            epoch,
        } = queue.finish(next, Err(anyhow::anyhow!("Storage unavailable")))
        else {
            anyhow::bail!("Failed save did not finish");
        };
        let saved = saved.ok_or_else(|| anyhow::anyhow!("Previous durable save was lost"))?;
        assert_eq!(saved.validated.threads, 4);
        assert_eq!(error.as_deref(), Some("Storage unavailable"));
        assert!(save_may_enable(epoch, 1, false));
        assert!(!save_may_enable(epoch, 2, false));
        assert!(queue.pending.is_none());
        assert!(queue.saved.is_none());
        Ok(())
    }

    #[test]
    fn save_completion_preserves_new_edits_and_a_later_pause() {
        let original = request(4, 5);
        let mut draft = original.draft.clone();
        let mut validated = original.draft.clone();
        validated.model = PathBuf::from("/resolved/model.gguf");
        let saved = SavedConfig {
            submitted: original,
            validated,
        };
        draft.threads = 8;
        assert!(!saved.update_draft(&mut draft));
        assert_eq!(draft.threads, 8);
        assert!(draft.model.as_os_str().is_empty());
        assert!(save_may_enable(5, 5, false));
        assert!(!save_may_enable(5, 6, false));
        // A deliberate Resume after Pause still receives the durable settings.
        assert!(save_may_enable(5, 6, true));
        draft = saved.submitted.draft.clone();
        assert!(saved.update_draft(&mut draft));
        assert!(draft == saved.validated);
    }

    #[test]
    fn save_worker_returns_before_storage_and_joins_owned_work() -> anyhow::Result<()> {
        let (entered, started) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let work = SaveWork::spawn(move || {
            entered.send(())?;
            released.recv()?;
            Ok(Config::default())
        })?;
        started.recv_timeout(Duration::from_secs(2))?;
        assert!(work.result.try_recv().is_err());
        let (joining, joined) = std::sync::mpsc::channel();
        let (finished, completion) = std::sync::mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            let _ = joining.send(());
            drop(work);
            let _ = finished.send(());
        });
        joined.recv_timeout(Duration::from_secs(2))?;
        assert!(completion.recv_timeout(Duration::from_millis(10)).is_err());
        release.send(())?;
        completion.recv_timeout(Duration::from_secs(2))?;
        cleanup
            .join()
            .map_err(|_| anyhow::anyhow!("Save cleanup failed"))?;
        Ok(())
    }

    #[test]
    fn settings_drop_finishes_current_write_before_latest_queued_save() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("engine");
        let model = directory.path().join("model.gguf");
        std::fs::write(&executable, b"fake engine")?;
        std::fs::write(&model, b"fake model")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = directory.path().join("settings.json");
        let first = Config {
            engine_executable: executable,
            model,
            threads: 4,
            ..Config::default()
        };
        let mut latest = first.clone();
        latest.threads = 8;
        let (entered, started) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let first_path = path.clone();
        let work = SaveWork::spawn(move || {
            entered.send(())?;
            released.recv()?;
            first.save(&first_path)?;
            Ok(first)
        })?;
        started.recv_timeout(Duration::from_secs(2))?;
        let view = Settings {
            config: latest.clone(),
            status: String::new(),
            notice: None,
            microphones: Vec::new(),
            dialog: None,
            devices: None,
            preview: None,
            setup: None,
            retiring_setups: Vec::new(),
            saving: None,
            save_work: Some(work),
            saves: SaveQueue {
                pending: Some(SaveRequest {
                    draft: latest,
                    epoch: 1,
                    path: path.clone(),
                }),
                saved: None,
            },
            progress: super::Progress::default(),
            _updates: gpui::Task::ready(()),
            demo: true,
            #[cfg(target_os = "windows")]
            _minimize: None,
        };
        let cleanup = std::thread::spawn(move || drop(view));
        release.send(())?;
        cleanup
            .join()
            .map_err(|_| anyhow::anyhow!("Settings cleanup failed"))?;
        assert_eq!(Config::read(&path)?.threads, 8);
        Ok(())
    }
}
