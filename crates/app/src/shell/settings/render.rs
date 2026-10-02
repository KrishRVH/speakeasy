//! The Settings element tree. Settings renders its draft configuration, so edits show here before
//! they are saved.

use std::path::Path;

use gpui::{
    AnyElement, App, Context, Div, FontWeight, KeyDownEvent, SharedString, Window, div, prelude::*,
    px, relative, rgb, svg,
};
use speakeasy_dictation::{
    config::Engine,
    setup::Progress,
    status::Indicator,
    theme::{Palette, mix},
};

use super::{Action, EngineFile, Setting, Settings, is_demo};
use crate::{WORDMARK_FONT, gpui_ext::alpha, icons::KEYSTONE, shell::Services};

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.config.theme.palette();
        let unconfigured = !is_demo(cx) && self.config.engine_executable.as_os_str().is_empty();
        let upgradable = !is_demo(cx)
            && self
                .bundled_engine
                .as_ref()
                .is_some_and(|engine| engine.upgrades(&self.config));
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
            .child(if self.is_setting_up() || unconfigured || upgradable {
                self.setup_card(palette, unconfigured, cx)
                    .into_any_element()
            } else {
                Self::gesture_hints(palette).into_any_element()
            })
            .when(!self.is_setting_up(), |settings| {
                settings.child(self.engine_section(palette, cx))
            })
            .child(self.microphone_section(palette, cx))
            .child(self.preferences_section(palette, cx))
            .child(self.commands(palette, cx))
            .child(self.status_line(palette))
            .child(footer(palette, cx))
    }
}

impl Settings {
    /// Setup chooses the engine, so manual engine controls wait until it ends.
    const fn is_setting_up(&self) -> bool {
        self.setup.is_some()
    }

    /// Setup for a new install, or an offer to replace an older automatic engine install with the
    /// faster build this app carries.
    fn setup_card(
        &self,
        palette: &'static Palette,
        unconfigured: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let progress = self.setup.as_ref().map(|(setup, _)| setup.progress());
        let (title, command) = if self.is_setting_up() {
            ("Setting up dictation", "Cancel")
        } else if unconfigured {
            ("Set up dictation", "Set up automatically")
        } else {
            ("A faster speech engine is ready", "Install it")
        };
        panel(palette)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().text_size(px(13.0)).child(title))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .child(setup_detail(progress, unconfigured)),
            )
            .when_some(progress, |card, progress| {
                card.child(progress_bar(progress, palette))
            })
            .child(
                div()
                    .flex()
                    .child(button(command, Action::Setup, palette, cx)),
            )
    }

    fn gesture_hints(palette: &'static Palette) -> impl IntoElement {
        let shortcut = speakeasy_platform::SHORTCUT;
        let cancel_shortcut = speakeasy_platform::CANCEL_SHORTCUT;
        panel(palette)
            .text_size(px(13.0))
            .line_height(px(23.0))
            .child(format!("Hold {shortcut} to speak."))
            .child(div().child(format!(
                "Add Space, or double-tap, for hands-free. {cancel_shortcut} cancels."
            )))
    }

    fn engine_section(&self, palette: &'static Palette, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(heading("LOCAL SPEECH", palette))
                    .child(button(
                        format!("Engine: {}", self.config.engine.label()),
                        Action::Engine,
                        palette,
                        cx,
                    )),
            )
            .child(file_row(
                &self.config.engine_executable,
                "Choose executable",
                EngineFile::Executable,
                palette,
                cx,
            ))
            .child(file_row(
                &self.config.model,
                "Choose model",
                EngineFile::Model,
                palette,
                cx,
            ))
    }

    fn microphone_section(
        &self,
        palette: &'static Palette,
        cx: &Context<Self>,
    ) -> impl IntoElement {
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
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(heading("MICROPHONE", palette))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(button(microphone, Action::Microphone, palette, cx))
                    .child(button("Refresh", Action::Refresh, palette, cx)),
            )
    }

    fn preferences_section(
        &self,
        palette: &'static Palette,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(heading("PREFERENCES", palette))
            .when(!self.is_setting_up(), |section| {
                section.child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(self.language_control(palette, cx))
                        .child(self.switch("Prefer GPU", Setting::PreferGpu, palette, cx)),
                )
            })
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(self.switch("Keep clipboard", Setting::KeepClipboard, palette, cx))
                    .child(self.switch("Reduce motion", Setting::ReduceMotion, palette, cx)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(self.switch("Remove um / uh", Setting::RemoveFillers, palette, cx))
                    .child(button(
                        format!("Theme: {}", self.config.theme.name()),
                        Action::Theme,
                        palette,
                        cx,
                    )),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
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
    fn switch(
        &self,
        label: &str,
        setting: Setting,
        palette: &'static Palette,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let state = if setting.is_on(&self.config) {
            "On"
        } else {
            "Off"
        };
        button(
            format!("{label}: {state}"),
            Action::Toggle(setting),
            palette,
            cx,
        )
    }

    fn language_control(&self, palette: &'static Palette, cx: &Context<Self>) -> AnyElement {
        if self.config.engine == Engine::Parakeet {
            return div()
                .py(px(8.0))
                .text_size(px(12.0))
                .child("Language: Automatic")
                .into_any_element();
        }
        let label: SharedString = match self.config.language.as_str() {
            "auto" => "Language: Auto".into(),
            "en" => "Language: English".into(),
            language => format!("Language: {language}").into(),
        };
        button(label, Action::Language, palette, cx).into_any_element()
    }

    fn commands(&self, palette: &'static Palette, cx: &Context<Self>) -> impl IntoElement {
        let running = cx.global::<Services>().running();
        let (label, action) = if is_demo(cx) {
            ("Replay preview", Action::Preview)
        } else if running {
            ("Save changes", Action::Save)
        } else {
            ("Enable dictation", Action::Save)
        };
        div()
            .flex()
            .gap(px(8.0))
            .when(!self.is_setting_up(), |row| {
                row.child(button(label, action, palette, cx))
            })
            .when(running, |row| {
                row.child(button("Pause", Action::Pause, palette, cx))
            })
            .child(button("Quit", Action::Quit, palette, cx))
    }

    fn status_line(&self, palette: &'static Palette) -> impl IntoElement {
        div()
            .text_size(px(12.0))
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

fn setup_detail(progress: Option<Progress>, unconfigured: bool) -> String {
    let Some(Progress { step, done, total }) = progress else {
        return if unconfigured {
            "Speakeasy installs its Metal speech engine and downloads the Parakeet model, about 0.7 GB, then turns dictation on."
        } else {
            "Speakeasy's own Metal build recognizes speech faster with the same model. Setup installs it and keeps your model."
        }
        .into();
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

fn file_row(
    path: &Path,
    label: &'static str,
    file: EngineFile,
    palette: &'static Palette,
    cx: &Context<Settings>,
) -> impl IntoElement {
    let name = path.file_name().map_or_else(
        || "Not selected".into(),
        |name| name.to_string_lossy().into_owned(),
    );
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
                .child(name),
        )
        .child(button(label, Action::Choose(file), palette, cx))
}

fn button(
    label: impl IntoElement,
    action: Action,
    palette: &'static Palette,
    cx: &Context<Settings>,
) -> impl IntoElement {
    div()
        .id(action.element_id())
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
        .hover(|style| style.bg(rgb(mix(palette.raise, palette.ink, 0.08))))
        .focus(|style| style.border_color(rgb(palette.lamp)))
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

fn panel(palette: &'static Palette) -> Div {
    div()
        .p(px(16.0))
        .rounded(px(4.0))
        .bg(rgb(palette.panel))
        .border_1()
        .border_color(alpha(palette.lamp, 0.14))
}

fn heading(text: &'static str, palette: &'static Palette) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .text_color(rgb(palette.muted))
        .child(text)
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
