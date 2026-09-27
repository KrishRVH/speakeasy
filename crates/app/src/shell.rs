use crate::{
    audio,
    config::{Config, Engine},
    pill::Pill,
    runtime::{Phase, Runtime, Snapshot},
};
use gpui::{prelude::*, *};
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
}
impl Global for Services {}

impl Services {
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
                snapshot.message = "Loading local model…".into();
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

pub fn open(cx: &mut App) -> anyhow::Result<()> {
    if let Some(window) = cx.global::<Services>().window
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return Ok(());
    }
    let config = cx.global::<Services>().config.clone();
    let demo = cx.global::<Services>().demo;
    let mut updates = cx.global::<Services>().output.subscribe();
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
        |_, cx| {
            cx.new(|cx: &mut Context<Settings>| {
                let task = cx.spawn(async move |this, cx| {
                    loop {
                        let snapshot = updates.borrow_and_update().clone();
                        let status = if !snapshot.message.is_empty() {
                            snapshot.message
                        } else {
                            match snapshot.phase {
                                Phase::Starting => "Opening microphone…".into(),
                                Phase::Recording | Phase::Stopping => "Listening".into(),
                                Phase::Processing => "Transcribing locally…".into(),
                                Phase::Cancelled => "Cancelled".into(),
                                Phase::Done => "Ready for your next thought".into(),
                                _ => "Ready when you are".into(),
                            }
                        };
                        if this
                            .update(cx, |view, cx| {
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
                };
                if !demo {
                    view.refresh_devices(cx);
                }
                view
            })
        },
    )?;
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
    Save,
    Pause,
    Preview,
    Quit,
}

impl Settings {
    fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        let enumeration = cx
            .background_executor()
            .spawn(async { audio::microphones() });
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
    fn choose(&mut self, model: bool, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                if self.config.engine == Engine::Parakeet && model {
                    "Choose a Parakeet v3 GGUF model"
                } else if self.config.engine == Engine::Parakeet {
                    "Choose nemo-speech"
                } else if model {
                    "Choose a Whisper GGML model"
                } else {
                    "Choose whisper-server"
                }
                .into(),
            ),
        });
        self.dialog = Some(cx.spawn(async move |this, cx| {
            let result = picker.await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            if model {
                                view.config.model = path;
                            } else {
                                view.config.engine_executable = path;
                            }
                            view.notice = Some("Unsaved changes".into());
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => view.notice = Some("Could not open the file picker. Try again.".into()),
                }
                view.dialog = None;
                cx.notify();
            });
        }));
    }
    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
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
            Action::Executable => self.choose(false, cx),
            Action::Model => self.choose(true, cx),
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
            snapshot.phase = Phase::Processing;
            tx.send_replace(snapshot.clone());
            Timer::after(Duration::from_millis(1400)).await;
            snapshot.phase = Phase::Done;
            tx.send_replace(snapshot);
        }));
    }
}
fn button(
    label: impl Into<SharedString>,
    action: Action,
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
        .rounded(px(7.0))
        .border_1()
        .border_color(rgb(0x35363d))
        .bg(rgb(0x23242a))
        .text_size(px(12.0))
        .hover(|s| s.bg(rgb(0x303138)))
        .focus(|s| s.border_color(rgb(0xff765d)))
        .cursor_pointer()
        .on_click(cx.listener(move |view, _, _, cx| view.act(action, cx)))
        .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                view.act(action, cx);
                cx.stop_propagation();
            }
        }))
        .child(label)
}
fn label(text: &'static str) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .text_color(rgb(0xa7a8b0))
        .child(text)
}
fn filename(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || "Not selected".into(),
        |name| name.to_string_lossy().into_owned(),
    )
}
impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let running = cx
            .global::<Services>()
            .runtime
            .as_ref()
            .is_some_and(Runtime::is_running);
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
            .bg(rgb(0x111215))
            .text_color(rgb(0xeaeaec))
            .p(px(32.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().size(px(8.0)).rounded_full().bg(rgb(0xff4f2e)))
                    .child(div().text_size(px(24.0)).child("Speakeasy")),
            )
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(rgb(0xa7a8b0))
                    .child("Your voice, right where you’re working."),
            )
            .child(
                div()
                    .p(px(16.0))
                    .rounded(px(10.0))
                    .bg(rgb(0x1a1b20))
                    .text_size(px(13.0))
                    .line_height(px(23.0))
                    .child("Hold Ctrl + Alt + Space to speak.")
                    .child(div().child("Double-tap for hands-free. Escape cancels.")),
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
                            .child(label("LOCAL SPEECH"))
                            .child(button(
                                match self.config.engine {
                                    Engine::Whisper => "Engine: Whisper",
                                    Engine::Parakeet => "Engine: Parakeet",
                                },
                                Action::Engine,
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
                            .child(button("Choose executable", Action::Executable, cx)),
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
                            .child(button("Choose model", Action::Model, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(label("MICROPHONE"))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(button(microphone, Action::Microphone, cx))
                            .child(button("Refresh", Action::Refresh, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(label("PREFERENCES"))
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
                                    cx,
                                ))
                                .child(button(
                                    if self.config.use_gpu {
                                        "Prefer GPU: On"
                                    } else {
                                        "Prefer GPU: Off"
                                    },
                                    Action::Gpu,
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
                                cx,
                            ))
                            .child(button(
                                if self.config.reduced_motion {
                                    "Reduce motion: On"
                                } else {
                                    "Reduce motion: Off"
                                },
                                Action::Motion,
                                cx,
                            )),
                    ),
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
                        cx,
                    ))
                    .when(running, |row| row.child(button("Pause", Action::Pause, cx)))
                    .child(button("Quit", Action::Quit, cx)),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(if self.notice.is_some() {
                        0xf2bd78
                    } else {
                        0xa7a8b0
                    }))
                    .child(self.notice.clone().unwrap_or_else(|| self.status.clone())),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(0x73747d))
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
