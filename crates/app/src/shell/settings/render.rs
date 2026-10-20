//! The Settings element tree. Settings renders its draft configuration, so edits show here before
//! they are saved.

use std::path::Path;

use gpui::{
    AnyElement, App, Context, Div, FontWeight, KeyDownEvent, Pixels, SharedString, Window, div,
    prelude::*, px, relative, rgb, svg,
};

use super::{Action, EngineFile, Setting, Settings, is_demo};
use crate::{
    WORDMARK_FONT,
    config::{Engine, TextDelivery},
    icons::KEYSTONE,
    setup::Progress,
    shell::Services,
    status::Indicator,
    theme::{Palette, alpha, mix},
};

/// The space between stacked rows and between the controls in a row.
const ROW_GAP: Pixels = px(8.0);
/// A control's vertical padding. A plain label pads the same, so a row keeps one height.
const CONTROL_PAD_Y: Pixels = px(8.0);
/// The size of control, detail and status text.
const TEXT: Pixels = px(12.0);

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.config.theme.palette();
        let ui = Ui { palette, cx };
        let unconfigured = !is_demo(cx) && self.config.needs_engine();
        div()
            .id("settings")
            .on_key_down(cycle_focus)
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(palette.room))
            .text_color(rgb(palette.ink))
            .p(px(32.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(header(self.presentation.indicator, palette))
            .child(if self.is_setting_up() || unconfigured {
                self.setup_card(&ui).into_any_element()
            } else {
                self.gesture_hints(&ui).into_any_element()
            })
            .when(!self.is_setting_up(), |settings| {
                settings.child(self.engine_section(&ui))
            })
            .when(cfg!(target_os = "linux"), |settings| {
                settings.child(self.linux_section(&ui))
            })
            .child(self.microphone_section(&ui))
            .child(self.preferences_section(&ui))
            .child(self.commands(&ui))
            .child(self.status_line(palette))
            .child(footer(palette, cx))
    }
}

/// What every control draws with: the draft's palette, and the view its actions reach.
struct Ui<'a> {
    palette: &'static Palette,
    cx: &'a Context<'a, Settings>,
}

impl Settings {
    /// Setup chooses the engine, so manual engine controls wait until it ends.
    const fn is_setting_up(&self) -> bool {
        self.setup.is_some()
    }

    fn setup_card(&self, ui: &Ui<'_>) -> impl IntoElement {
        let progress = self.setup.as_ref().map(|(setup, _)| setup.progress());
        let (title, command) = if self.is_setting_up() {
            ("Setting up dictation", "Cancel")
        } else {
            ("Set up dictation", "Set up automatically")
        };
        ui.panel()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().text_size(px(13.0)).child(title))
            .child(
                div()
                    .text_size(TEXT)
                    .text_color(rgb(ui.palette.muted))
                    .child(setup_detail(progress)),
            )
            .when_some(progress, |card, progress| {
                card.child(progress_bar(progress, ui.palette))
            })
            .child(div().flex().child(ui.button(command, Action::Setup)))
    }

    fn gesture_hints(&self, ui: &Ui<'_>) -> impl IntoElement {
        let shortcut = &self.presentation.shortcut;
        let cancel_shortcut = &self.presentation.cancel_shortcut;
        let desktop_bindings = cfg!(target_os = "linux") && self.config.linux.external_shortcut;
        let hold_hint = if desktop_bindings {
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
        ui.panel()
            .text_size(px(13.0))
            .line_height(px(23.0))
            .child(hold_hint)
            .child(div().child(gesture_hint))
    }

    fn engine_section(&self, ui: &Ui<'_>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(ROW_GAP)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(ui.heading("LOCAL SPEECH"))
                    .child(ui.button(
                        format!("Engine: {}", self.config.engine.label()),
                        Action::Engine,
                    )),
            )
            .child(ui.file_row(
                &self.config.engine_executable,
                "Choose executable",
                EngineFile::Executable,
            ))
            .child(ui.file_row(&self.config.model, "Choose model", EngineFile::Model))
    }

    fn linux_section(&self, ui: &Ui<'_>) -> impl IntoElement {
        let shortcut = &self.presentation.shortcut;
        let cancel_shortcut = &self.presentation.cancel_shortcut;
        let linux = &self.config.linux;
        // Only Linux shows this section; elsewhere it still compiles.
        #[cfg(target_os = "linux")]
        let command = &self.desktop_command;
        #[cfg(not(target_os = "linux"))]
        let command = "speakeasy";
        ui.panel()
            .flex()
            .flex_col()
            .gap(ROW_GAP)
            .child(ui.heading("LINUX DESKTOP"))
            .child(div().text_size(TEXT).child(format!("Shortcut: {shortcut}")))
            .child(div().text_size(TEXT).child(format!("Cancel: {cancel_shortcut}")))
            .child(ui.button(
                if linux.external_shortcut {
                    "Shortcuts: Desktop bindings"
                } else {
                    "Shortcuts: Native desktop"
                },
                Action::Toggle(Setting::DesktopBindings),
            ))
            .child(div().text_size(TEXT).child(format!(
                "Desktop bindings: assign {command} --toggle and {command} --cancel in your desktop's shortcut settings."
            )))
    }

    fn microphone_section(&self, ui: &Ui<'_>) -> impl IntoElement {
        let microphone: SharedString = match &self.config.microphone {
            None => "System default".into(),
            Some(id) => self
                .microphones
                .iter()
                .find(|device| &device.id == id)
                .map_or_else(
                    || "Unavailable · choose another".into(),
                    |device| device.name.clone().into(),
                ),
        };
        ui.section("MICROPHONE").child(
            row()
                .child(ui.button(microphone, Action::Microphone))
                .child(ui.button("Refresh", Action::Refresh)),
        )
    }

    fn preferences_section(&self, ui: &Ui<'_>) -> impl IntoElement {
        ui.section("PREFERENCES")
            .when(!self.is_setting_up(), |section| {
                section.child(row().child(self.language_control(ui)).child(self.switch(
                    ui,
                    "Prefer GPU",
                    Setting::PreferGpu,
                )))
            })
            .child(row().child(self.delivery_control(ui)).child(self.switch(
                ui,
                "Reduce motion",
                Setting::ReduceMotion,
            )))
            .child(
                row()
                    .child(self.switch(ui, "Remove um / uh", Setting::RemoveFillers))
                    .child(ui.button(
                        format!("Theme: {}", self.config.theme.name()),
                        Action::Theme,
                    )),
            )
            .child(
                div()
                    .text_size(TEXT)
                    .text_color(rgb(ui.palette.muted))
                    .child(
                        if self.config.engine == Engine::Whisper
                            && !self.config.language.eq_ignore_ascii_case("en")
                        {
                            "Select English to remove um / uh."
                        } else {
                            "English fillers. Turn off when dictating other languages."
                        },
                    ),
            )
    }

    /// A button that toggles `setting`, labeled with its draft state.
    fn switch(&self, ui: &Ui<'_>, label: &str, setting: Setting) -> impl IntoElement {
        let state = if setting.is_on(&self.config) {
            "On"
        } else {
            "Off"
        };
        ui.button(format!("{label}: {state}"), Action::Toggle(setting))
    }

    /// How dictated text reaches the editor. Windows and macOS offer paste or typing as a switch;
    /// Linux adds its terminal paste keys and copying without keyboard control, and its labels keep
    /// the names that platform remedies tell users to choose.
    fn delivery_control(&self, ui: &Ui<'_>) -> impl IntoElement {
        let label = match self.config.text_delivery() {
            #[cfg(not(target_os = "linux"))]
            TextDelivery::Paste => "Keep clipboard: Off",
            #[cfg(not(target_os = "linux"))]
            TextDelivery::Type => "Keep clipboard: On",
            #[cfg(target_os = "linux")]
            TextDelivery::Paste => "Insert: Paste (Ctrl+V)",
            #[cfg(target_os = "linux")]
            TextDelivery::TerminalPaste => "Insert: Paste (Ctrl+Shift+V)",
            #[cfg(target_os = "linux")]
            TextDelivery::Type => "Insert: Type (Keep clipboard)",
            #[cfg(target_os = "linux")]
            TextDelivery::Copy => "Insert: Copy for manual paste",
        };
        ui.button(label, Action::Delivery)
    }

    fn language_control(&self, ui: &Ui<'_>) -> AnyElement {
        if self.config.engine == Engine::Parakeet {
            return div()
                .py(CONTROL_PAD_Y)
                .text_size(TEXT)
                .child("Language: Automatic")
                .into_any_element();
        }
        let label: SharedString = match self.config.language.as_str() {
            "auto" => "Language: Auto".into(),
            "en" => "Language: English".into(),
            language => format!("Language: {language}").into(),
        };
        ui.button(label, Action::Language).into_any_element()
    }

    fn commands(&self, ui: &Ui<'_>) -> impl IntoElement {
        let running = ui.cx.global::<Services>().running();
        let (label, action) = if is_demo(ui.cx) {
            ("Replay preview", Action::Preview)
        } else if running {
            ("Save changes", Action::Save)
        } else {
            ("Enable dictation", Action::Save)
        };
        row()
            .when(!self.is_setting_up(), |row| {
                row.child(ui.button(label, action))
            })
            .when(running, |row| row.child(ui.button("Pause", Action::Pause)))
            .child(ui.button("Quit", Action::Quit))
    }

    fn status_line(&self, palette: &'static Palette) -> impl IntoElement {
        div()
            .text_size(TEXT)
            .text_color(rgb(if self.notice.is_some() {
                palette.warn
            } else {
                palette.muted
            }))
            .child(self.presentation.message.clone())
            .when_some(self.notice.clone(), |status, notice| {
                status.child(div().child(notice))
            })
    }
}

impl Ui<'_> {
    fn button(&self, label: impl IntoElement, action: Action) -> impl IntoElement {
        let palette = self.palette;
        div()
            .id(action.element_id())
            .focusable()
            .tab_stop(true)
            .px(px(12.0))
            .max_w(px(390.0))
            .overflow_hidden()
            .text_ellipsis()
            .py(CONTROL_PAD_Y)
            .rounded(px(3.0))
            .border_1()
            .border_color(rgb(mix(palette.raise, palette.ink, 0.14)))
            .bg(rgb(palette.raise))
            .text_size(TEXT)
            .hover(|style| style.bg(rgb(mix(palette.raise, palette.ink, 0.08))))
            .focus(|style| style.border_color(rgb(palette.lamp)))
            .cursor_pointer()
            .on_click(
                self.cx
                    .listener(move |view, _, window, cx| view.act(action, window, cx)),
            )
            .on_key_down(
                self.cx
                    .listener(move |view, event: &KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            view.act(action, window, cx);
                            cx.stop_propagation();
                        }
                    }),
            )
            .child(label)
    }

    fn file_row(&self, path: &Path, label: &'static str, file: EngineFile) -> impl IntoElement {
        let name = path.file_name().map_or_else(
            || "Not selected".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(div().flex_1().overflow_hidden().text_size(TEXT).child(name))
            .child(self.button(label, Action::Choose(file)))
    }

    fn panel(&self) -> Div {
        div()
            .p(px(16.0))
            .rounded(px(4.0))
            .bg(rgb(self.palette.panel))
            .border_1()
            .border_color(alpha(self.palette.lamp, 0.14))
    }

    fn heading(&self, text: &'static str) -> impl IntoElement {
        div()
            .text_size(TEXT)
            .text_color(rgb(self.palette.muted))
            .child(text)
    }

    /// A titled stack of rows.
    fn section(&self, title: &'static str) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(ROW_GAP)
            .child(self.heading(title))
    }
}

/// Controls side by side.
fn row() -> Div {
    div().flex().gap(ROW_GAP)
}

fn setup_detail(progress: Option<Progress>) -> String {
    let Some(Progress { step, done, total }) = progress else {
        return format!(
            "Speakeasy downloads the Parakeet speech model and the engine that suits this {}, about 0.8 GB, then turns dictation on.",
            if cfg!(target_os = "macos") {
                "Mac"
            } else {
                "PC"
            }
        );
    };
    match step {
        Some(step) if total > 0 => {
            format!("{step} · {} of {} MB", done / 1_000_000, total / 1_000_000)
        },
        Some(step) => format!("{step}…"),
        None => "Checking this machine…".into(),
    }
}

fn cycle_focus(event: &KeyDownEvent, window: &mut Window, cx: &mut App) {
    if event.keystroke.key == "tab" {
        if event.keystroke.modifiers.shift {
            window.focus_prev();
        } else {
            window.focus_next();
        }
        cx.stop_propagation();
    }
}

fn header(indicator: Indicator, palette: &'static Palette) -> impl IntoElement {
    let indicator_color = match indicator {
        Indicator::Recording => palette.live,
        Indicator::Attention => palette.warn,
        Indicator::Ready => palette.lamp,
        Indicator::Busy | Indicator::Paused => palette.faint(),
    };
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
        .child(keystone_rule(palette))
}

fn footer(palette: &'static Palette, cx: &App) -> impl IntoElement {
    div()
        .text_size(px(11.0))
        .text_color(rgb(palette.faint()))
        .child(if is_demo(cx) {
            "Preview only · Simulated audio · No insertion"
        } else {
            "On your device · No account · No history"
        })
}

fn progress_bar(progress: Progress, palette: &'static Palette) -> impl IntoElement {
    div()
        .h(px(3.0))
        .rounded(px(2.0))
        .bg(alpha(palette.lamp, 0.16))
        .child(
            div()
                .h_full()
                .rounded(px(2.0))
                .bg(rgb(palette.lamp))
                .w(relative(
                    progress.done as f32 / progress.total.max(1) as f32,
                )),
        )
}

fn jewel(color: u32, size: f32) -> impl IntoElement {
    svg()
        .path(KEYSTONE)
        .size(px(size))
        .text_color(rgb(color))
        .flex_none()
}

// GPUI text has no letter spacing, so the wordmark lays its capitals out as glyphs.
fn wordmark(palette: &'static Palette) -> impl IntoElement {
    div()
        .flex()
        .gap(px(5.0))
        .font_family(WORDMARK_FONT)
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
