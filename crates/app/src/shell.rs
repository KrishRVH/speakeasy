use crate::{
    audio,
    config::{Config, Engine},
    pill::Pill,
    runtime::{ModelState, Phase, Runtime, Snapshot},
    status,
    theme::{Palette, alpha, mix},
};
use gpui::{prelude::*, *};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use raw_window_handle::HasWindowHandle;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use speakeasy_platform::Input;
use speakeasy_platform::InputMonitor;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::watch;

pub struct Services {
    pub monitor: Option<InputMonitor>,
    pub runtime: Option<Runtime>,
    // A paused owner stays here until native cleanup completes. Its task only
    // waits for acknowledgement; dropping Services still joins the runtime.
    pub retiring: Option<Runtime>,
    pub retirement: Option<Task<()>>,
    pub pending: Option<Config>,
    pub path: PathBuf,
    pub config: Config,
    pub output: watch::Sender<Snapshot>,
    pub pill: WindowHandle<Pill>,
    pub window: Option<WindowHandle<Settings>>,
    pub demo: bool,
    pub demo_tray: bool,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub visibility: Option<Task<()>>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub tray_hint_seen: bool,
    pub _reopen: Option<Task<()>>,
    pub _instance: Option<crate::instance::Instance>,
}
impl Global for Services {}

impl Services {
    pub fn running(&self) -> bool {
        self.runtime.as_ref().is_some_and(Runtime::is_running)
    }
    pub fn apply(&mut self, config: Config, cx: &mut App) -> anyhow::Result<()> {
        if self
            .runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.is_running())
        {
            self.stop(cx);
        }
        if self.retiring.is_some() {
            self.config = config.clone();
            self.pending = Some(config);
            return Ok(());
        }
        if let Some(runtime) = &self.runtime {
            runtime.configure(config.clone());
        } else {
            self.output.send_modify(|snapshot| {
                snapshot.phase = Phase::Idle;
                snapshot.model = ModelState::Loading;
                snapshot.message = crate::runtime::LOADING.into();
            });
            let runtime = Runtime::start(config.clone(), self.output.clone())?;
            let monitor = match InputMonitor::start(runtime.input.clone()) {
                Ok(monitor) => monitor,
                Err(error) => {
                    drop(runtime);
                    self.output.send_modify(|snapshot| {
                        snapshot.phase = Phase::Error;
                        snapshot.message = error.to_string();
                    });
                    return Err(error);
                }
            };
            self.monitor = Some(monitor);
            self.runtime = Some(runtime);
        }
        self.config = config;
        Ok(())
    }
    pub fn stop(&mut self, cx: &mut App) {
        if let Some(runtime) = &self.runtime {
            runtime.request_stop();
        }
        self.monitor.take();
        self.pending = None;
        if let Some(runtime) = self.runtime.take() {
            let stopped = runtime.stopped();
            self.retiring = Some(runtime);
            self.retirement = Some(cx.spawn(async move |cx| {
                stopped.await;
                let _ = cx.update(|cx| {
                    cx.update_global::<Services, _>(|services, cx| {
                        // The owner has stopped publishing and released its
                        // worker. Joining here no longer waits for native work.
                        services.retiring.take();
                        if let Some(config) = services.pending.take() {
                            if let Err(error) = services.apply(config, cx) {
                                services.output.send_modify(|snapshot| {
                                    snapshot.phase = Phase::Error;
                                    snapshot.message = error.to_string();
                                });
                            }
                        } else {
                            services.output.send_replace(Snapshot {
                                message: "Dictation paused".into(),
                                ..Snapshot::default()
                            });
                        }
                        // Release our task handle after all completion work;
                        // nothing after this point may await further work.
                        services.retirement.take();
                    });
                });
            }));
        }
        self.output.send_replace(Snapshot {
            message: if self.retiring.is_some() {
                "Pausing dictation…"
            } else {
                "Dictation paused"
            }
            .into(),
            ..Snapshot::default()
        });
    }
}

pub fn reveal(cx: &mut App) {
    if !cx.has_global::<Services>() {
        return;
    }
    if let Err(error) = open(cx) {
        speakeasy_platform::show_error(&error.to_string());
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn toggle_enabled(cx: &mut App) {
    if cx.global::<Services>().demo {
        return;
    }
    let result = cx.update_global::<Services, _>(|services, cx| {
        if services.retiring.is_some() {
            return Ok(());
        }
        if services.running() {
            services.stop(cx);
            Ok(())
        } else {
            let mut config = services.config.clone();
            config
                .validate(&services.path)
                .and_then(|()| services.apply(config, cx))
        }
    });
    if let Err(error) = result {
        cx.global::<Services>().output.send_modify(|snapshot| {
            snapshot.phase = Phase::Error;
            snapshot.message = error.to_string();
        });
        reveal(cx);
    }
}

// Resolve native handles immediately before use, outside GPUI's window borrow.
// Replacing the task cancels a queued hide if the app is reopened first.
#[cfg(any(target_os = "windows", target_os = "macos"))]
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
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        settings_visible(window, true, cx);
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let _ = window.update(cx, |_, window, _| window.activate_window());
        return Ok(());
    }
    let config = cx.global::<Services>().config.clone();
    let demo = cx.global::<Services>().demo;
    let tray_lifecycle = !demo || cx.global::<Services>().demo_tray;
    let mut updates = cx.global::<Services>().output.subscribe();
    #[cfg(target_os = "windows")]
    let mut native_result = Ok(());
    let window = cx.open_window(
        WindowOptions {
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
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let _ = (window, tray_lifecycle);
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            if tray_lifecycle {
                window.on_window_should_close(cx, |_, cx| {
                    if !cx.has_global::<Services>() {
                        return true;
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
                    loop {
                        let snapshot = updates.borrow_and_update().clone();
                        if this
                            .update(cx, |view, cx| {
                                let services = cx.global::<Services>();
                                let status = if !snapshot.message.is_empty() {
                                    snapshot.message.clone()
                                } else {
                                    status::description(
                                        &snapshot,
                                        services.running() || services.demo,
                                        services.retiring.is_some(),
                                        services.config.engine,
                                    )
                                };
                                if view.status != status {
                                    view.status = status;
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
                    _updates: task,
                    demo,
                    #[cfg(target_os = "windows")]
                    _minimize: minimize,
                };
                if !demo {
                    view.refresh_devices(cx);
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

pub struct Settings {
    config: Config,
    status: String,
    notice: Option<String>,
    microphones: Vec<(String, String)>,
    dialog: Option<Task<()>>,
    devices: Option<Task<()>>,
    preview: Option<Task<()>>,
    _updates: Task<()>,
    demo: bool,
    #[cfg(target_os = "windows")]
    _minimize: Option<Task<()>>,
}
#[derive(Clone, Copy)]
enum Action {
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
        match action {
            Action::Engine => {
                self.config.engine = match self.config.engine {
                    Engine::Whisper => Engine::Parakeet,
                    Engine::Parakeet => Engine::Whisper,
                };
                if self.config.engine == Engine::Parakeet {
                    self.config.use_gpu = true;
                }
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
                if self.config.engine == Engine::Parakeet {
                    return;
                }
                self.config.use_gpu = !self.config.use_gpu;
                self.notice = Some("Unsaved changes".into());
            }
            Action::Clipboard => {
                self.config.preserve_clipboard = !self.config.preserve_clipboard;
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
                    let path = cx.global::<Services>().path.clone();
                    let result = self
                        .config
                        .validate(&path)
                        .and_then(|()| self.config.save(&path))
                        .and_then(|()| {
                            cx.update_global::<Services, _>(|services, cx| {
                                services.apply(self.config.clone(), cx)
                            })
                        });
                    self.notice = match result {
                        Ok(()) => None,
                        Err(error) => Some(error.to_string()),
                    };
                    let pill = cx.global::<Services>().pill;
                    let reduced =
                        self.config.reduced_motion || speakeasy_platform::reduced_motion();
                    let _ = pill.update(cx, |view, _, cx| {
                        view.set_reduced(reduced);
                        cx.notify();
                    });
                    show_theme(cx.global::<Services>().config.theme, cx);
                }
            }
            Action::Pause => {
                cx.update_global::<Services, _>(|services, cx| services.stop(cx));
                self.notice = None;
            }
            Action::Preview => self.play(cx),
            Action::Quit => cx.quit(),
        }
        cx.notify();
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
fn label(text: &'static str, palette: &'static Palette) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .text_color(rgb(palette.muted))
        .child(text)
}
// A cut-jewel status mark, matching the tray badges.
fn jewel(color: u32, size: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            crate::pill::keystone(
                window,
                f32::from(center.x),
                f32::from(center.y),
                size / 2.0,
                rgb(color),
            );
        },
    )
    .size(px(size))
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
        let indicator = {
            let services = cx.global::<Services>();
            status::indicator(
                &services.output.borrow(),
                services.running() || services.demo,
                services.retiring.is_some(),
            )
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
                div()
                    .p(px(16.0))
                    .rounded(px(4.0))
                    .bg(rgb(palette.panel))
                    .border_1()
                    .border_color(alpha(palette.lamp, 0.14))
                    .text_size(px(13.0))
                    .line_height(px(23.0))
                    .child(format!("Hold {} to speak.", speakeasy_platform::SHORTCUT))
                    .child(
                        div().child("Add Space, or double-tap, for hands-free. Escape cancels."),
                    ),
            )
            .child(
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
                            .child(button("Choose executable", Action::Executable, palette, cx)),
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
                    .child(div().flex().gap(px(8.0)).child(
                        if self.config.engine == Engine::Parakeet {
                            div()
                                .py(px(8.0))
                                .text_size(px(12.0))
                                .child("Language: Automatic · GPU required")
                                .into_any_element()
                        } else {
                            div()
                                .flex()
                                .gap(px(8.0))
                                .child(button(
                                    match self.config.language.as_str() {
                                        "auto" => "Language: Auto".to_owned(),
                                        "en" => "Language: English".to_owned(),
                                        language => format!("Language: {language}"),
                                    },
                                    Action::Language,
                                    palette,
                                    cx,
                                ))
                                .child(button(
                                    if self.config.use_gpu {
                                        "Prefer GPU: On"
                                    } else {
                                        "Prefer GPU: Off"
                                    },
                                    Action::Gpu,
                                    palette,
                                    cx,
                                ))
                                .into_any_element()
                        },
                    ))
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
                    .child(button(
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

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn send(action: Input, cx: &App) {
    if let Some(runtime) = &cx.global::<Services>().runtime {
        speakeasy_platform::deliver(&runtime.input, action);
    }
}
