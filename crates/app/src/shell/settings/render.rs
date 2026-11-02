mod onboarding;

use std::path::Path;

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, KeyDownEvent, Pixels, SharedString, Window, div,
    prelude::*, px, relative, rgb,
};

use super::{
    Action, EngineFile, Setting, Settings, is_demo, language, navigation::Page, permissions,
};
use crate::{
    config::{Engine, TextDelivery},
    controls::{Kit, Weight, facet},
    models::Model,
    setup::Progress,
    shell::Services,
    status::{Indicator, ServiceState},
    theme::{Palette, alpha},
};

const ROW_GAP: Pixels = px(8.0);

const TEXT: Pixels = px(12.0);

const EDGE: Pixels = px(32.0);

const STATUS_LINE: Pixels = px(18.0);

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.vocabulary.theme(self.config.theme, cx);
        self.recognition.theme(self.config.theme, cx);
        self.presets.theme(self.config.theme, cx);
        let palette = self.config.theme.palette();
        let ui = Ui { palette, cx };
        if let Some(step) = self.onboarding {
            return self.onboarding_view(step, &ui);
        }
        let unconfigured = !is_demo(cx) && self.config.needs_engine();
        let unsaved = self.unsaved(cx);
        let compact = window.viewport_size().height < px(600.0);
        let top = div()
            .flex_none()
            .p(EDGE)
            .pb(px(16.0))
            .when(compact, |top| top.pt(px(16.0)).pb(px(12.0)))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(header(self.presentation.indicator, palette, compact))
            .child(self.navigation(&ui));
        let body = div()
            .id("settings")
            .track_scroll(&self.navigation.scroll)
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .p(EDGE)
            .pt(px(0.0))
            .pb(px(24.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .when(self.is_setting_up() || unconfigured, |body| {
                body.child(self.setup_card(&ui))
            })
            .child(self.page(&ui))
            .child(footer(palette, cx));

        let bar = div()
            .flex_none()
            .flex()
            .flex_col()
            .child(ui.kit().double_rule())
            .child(
                div()
                    .px(EDGE)
                    .pt(px(14.0))
                    .pb(px(20.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(self.commands(&ui, unsaved))
                    .child(self.status_line(palette, unsaved)),
            );
        div()
            .on_key_down(cycle_focus)
            .size_full()
            .bg(rgb(palette.room))
            .text_color(rgb(palette.ink))
            .flex()
            .flex_col()
            .child(top)
            .child(body)
            .child(bar)
    }
}

struct Ui<'a> {
    palette: &'static Palette,
    cx: &'a Context<'a, Settings>,
}

impl Settings {
    fn navigation(&self, ui: &Ui<'_>) -> impl IntoElement {
        ui.kit()
            .tab_bar("settings-tabs", "Settings pages")
            .children([Page::Dictation, Page::Words, Page::Advanced].map(|page| {
                let action = Action::Page(page);
                ui.kit()
                    .tab(
                        action.element_id(),
                        page.label(),
                        self.navigation.page == page,
                        Some(ui.press(action)),
                    )
                    .track_focus(self.navigation.focus(page))
            }))
    }

    fn page(&self, ui: &Ui<'_>) -> impl IntoElement {
        let group = || div().flex().flex_col().gap(px(20.0));
        let model = if is_demo(ui.cx) {
            "Simulated model"
        } else if self.is_setting_up() {
            "Setting up local speech…"
        } else if self.config.needs_engine() {
            "Not set up"
        } else {
            self.config
                .model_choice()
                .map_or_else(|| self.config.engine.label(), Model::label)
        };
        let groups = match self.navigation.page {
            Page::Dictation => [
                Some(
                    group()
                        .child(self.readiness_card(ui))
                        .child(self.gesture_hints(ui, false)),
                ),
                Some(
                    group()
                        .child(self.microphone_section(ui))
                        .child(
                            ui.section("Local speech")
                                .child(
                                    row().items_center().justify_between().child(model).child(
                                        ui.button("Change model", Action::Page(Page::Advanced)),
                                    ),
                                )
                                .when(!self.is_setting_up(), |section| {
                                    section.child(self.language_control(ui))
                                }),
                        )
                        .child(self.presets_section(ui.cx)),
                ),
                Some(group().child(self.preferences_section(ui))),
            ],
            Page::Words => [
                (self.config.uses_nemotron()
                    || !self.config.recognition_terms.phrases().is_empty())
                .then(|| group().child(self.recognition_section(ui.cx))),
                Some(group().child(self.vocabulary_section(ui.cx))),
                None,
            ],
            Page::Advanced => [
                (!self.is_setting_up()).then(|| {
                    group().child(self.engine_section(ui)).child(self.switch(
                        ui,
                        "Prefer GPU",
                        Setting::PreferGpu,
                    ))
                }),
                Some(
                    group()
                        .when(cfg!(target_os = "linux"), |group| {
                            group.child(self.linux_section(ui))
                        })
                        .child(self.gesture_hints(ui, true)),
                ),
                Some(
                    group()
                        .child(self.update_section(ui))
                        .child(row().child(ui.button(
                            "Replay setup",
                            Action::Onboarding(super::onboarding::Action::Replay),
                        ))),
                ),
            ],
        };
        groups.into_iter().flatten().enumerate().fold(
            div().flex().flex_col().gap(px(24.0)),
            |page, (index, group)| {
                page.when(index > 0, |page| page.child(ui.kit().keystone_rule()))
                    .child(group)
            },
        )
    }

    fn update_section(&self, ui: &Ui<'_>) -> impl IntoElement {
        let controls = row()
            .child(ui.button(
                if self.update.busy() {
                    "Working…"
                } else {
                    "Check for updates"
                },
                Action::CheckUpdates,
            ))
            .when(self.update.version().is_some(), |row| {
                row.child(ui.button("Open release", Action::OpenUpdate))
            });
        #[cfg(target_os = "windows")]
        let controls = match &self.update {
            super::Update::Available(_) if crate::updates::Download::available() => {
                controls.child(ui.button("Download update", Action::DownloadUpdate))
            },
            super::Update::Downloading {
                cancelled: false, ..
            } => controls.child(ui.button("Cancel download", Action::CancelUpdate)),
            super::Update::Ready(_) => {
                controls.child(ui.button("Install and restart", Action::InstallUpdate))
            },
            _ => controls,
        };
        ui.section("Updates")
            .child(format!("Speakeasy {}", env!("CARGO_PKG_VERSION")))
            .child(controls)
            .child("Checks GitHub only when you ask. Installation stays in your control.")
    }

    fn readiness_card(&self, ui: &Ui<'_>) -> impl IntoElement {
        let readiness = self.presentation.readiness;
        let active = self.practice.task.is_some();
        let may_practice = ui.cx.global::<Services>().is_running() && !is_demo(ui.cx);
        let access = if is_demo(ui.cx) {
            "Demo: microphone, shortcut, and screen activity are simulated."
        } else {
            "Permission status refreshes when you return here. Try dictation to check your microphone and shortcut. Screen capture starts only when you ask for a Show."
        };
        let delivery = "To check delivery, end practice and dictate into an editor. Practice clears when this window hides or loses focus.";
        ui.kit()
            .inlay()
            .flex()
            .flex_col()
            .gap(ROW_GAP)
            .text_size(TEXT)
            .child(ui.kit().eyebrow("Dictation status"))
            .child(format!(
                "Model: {} · Shortcut: {}",
                readiness.model, readiness.shortcut
            ))
            .when_some(self.permissions, |card, permissions| {
                card.child(Self::permission_rows(ui, permissions))
            })
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(ui.palette.muted))
                    .child(access),
            )
            .child(row().when(may_practice, |row| {
                row.child(ui.button(
                    if active {
                        "End practice"
                    } else {
                        "Try dictation here"
                    },
                    Action::Practice,
                ))
            }))
            .when(active, |card| {
                card.child(if self.practice.ready {
                    format!(
                        "Hold {} and speak. Your words stay here; nothing is pasted or saved.",
                        self.presentation.shortcut
                    )
                } else {
                    "Preparing practice…".into()
                })
                .when(!self.practice.text.is_empty(), |card| {
                    card.child(
                        div()
                            .max_h(px(140.0))
                            .id("practice-result")
                            .overflow_y_scroll()
                            .child(gpui::text!(
                                id = "practice-words",
                                self.practice.text.to_string()
                            )),
                    )
                })
            })
            .when(active, |card| {
                card.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(ui.palette.muted))
                        .child(delivery),
                )
            })
    }

    fn permission_rows(
        ui: &Ui<'_>,
        permissions: speakeasy_platform::Permissions,
    ) -> impl IntoElement {
        let permission = |name, state, help| {
            row()
                .justify_between()
                .child(format!("{name}: {}", permissions::description(state)))
                .child(ui.button(
                    if cfg!(target_os = "linux") {
                        "Microphone help"
                    } else {
                        "Open settings"
                    },
                    Action::PermissionHelp(help),
                ))
        };
        let rows = div().flex().flex_col().gap(ROW_GAP).child(permission(
            "Microphone permission",
            permissions.microphone,
            permissions::Help::Microphone,
        ));
        #[cfg(target_os = "macos")]
        let rows = rows.child(permission(
            "Accessibility permission",
            permissions.accessibility,
            permissions::Help::Accessibility,
        ));
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let rows = rows.child(permission(
            "Screen permission (Shows only)",
            permissions.screen,
            permissions::Help::Screen,
        ));
        rows
    }

    const fn is_setting_up(&self) -> bool {
        self.setup.is_some()
    }

    fn setup_card(&self, ui: &Ui<'_>) -> impl IntoElement {
        let progress = self.setup.as_ref().map(|(setup, _, _)| setup.progress());
        let (title, command) = if self.is_setting_up() {
            ("Setting up dictation", "Cancel")
        } else {
            ("Set up dictation", "Set up automatically")
        };
        ui.kit()
            .inlay()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(ui.kit().eyebrow(title))
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

    fn gesture_hints(&self, ui: &Ui<'_>, detailed: bool) -> impl IntoElement {
        let alternative_hint = "Hold briefly before adding Space, S, W, or A. Quick combinations keep their usual action.";
        let shortcut = &self.presentation.shortcut;
        let cancel_shortcut = &self.presentation.cancel_shortcut;
        let desktop_bindings = cfg!(target_os = "linux") && self.config.linux.external_shortcut;
        let records_shows = !cfg!(target_os = "linux");
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
        ui.kit()
            .inlay()
            .text_size(px(13.0))
            .line_height(px(23.0))
            .child(hold_hint)
            .child(div().child(gesture_hint))
            .map(|panel| {
                #[cfg(target_os = "macos")]
                let panel = panel.child(row().child(ui.picker(
                    "Shortcut",
                    self.config.mac_shortcut.label(),
                    Action::MacShortcut,
                )));
                panel
            })
            .when(
                cfg!(target_os = "macos")
                    && self.config.mac_shortcut != speakeasy_platform::MacShortcut::Function,
                |panel| panel.child(alternative_hint),
            )
            .when(records_shows && detailed, |panel| {
                panel.children(
                    [
                        "For a Show, add S screen · W window · A area.".into(),
                        "To draw, hold Shift and move the pointer without clicking.".into(),
                        "Double-tap Shift to pin.".into(),
                        format!("To finish a Show, press and release {shortcut} again.").into(),
                        format!("To cancel a Show, hold {shortcut} and press Escape.").into(),
                    ]
                    .map(|line: SharedString| div().child(line)),
                )
            })
    }

    fn engine_section(&self, ui: &Ui<'_>) -> impl IntoElement {
        let detail = "Parakeet: 714 MB · 25 European languages. Nemotron: 742 MB · 32 locales, including Hindi, Japanese, Korean, and Mandarin. Downloads stay local; save to switch.";
        div()
            .flex()
            .flex_col()
            .gap(ROW_GAP)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(ui.kit().eyebrow("Local speech"))
                    .child(ui.picker("Engine", self.config.engine.label(), Action::Engine)),
            )
            .child(
                row()
                    .child(ui.button("Set up Parakeet v3", Action::Install(Model::Parakeet)))
                    .child(ui.button("Set up Nemotron 3.5", Action::Install(Model::Nemotron))),
            )
            .child(
                div()
                    .text_size(TEXT)
                    .text_color(rgb(ui.palette.muted))
                    .child(detail),
            )
            .when_some(self.config.model_choice(), |section, model| {
                section.child(format!("Selected model: {}", model.label()))
            })
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

        #[cfg(target_os = "linux")]
        let command = &self.desktop_command;
        #[cfg(not(target_os = "linux"))]
        let command = "speakeasy";
        ui.kit()
            .inlay()
            .flex()
            .flex_col()
            .gap(ROW_GAP)
            .child(ui.kit().eyebrow("Linux desktop"))
            .child(div().text_size(TEXT).child(format!("Shortcut: {shortcut}")))
            .child(div().text_size(TEXT).child(format!("Cancel: {cancel_shortcut}")))
            .child({
                let mode = if linux.external_shortcut {
                    "Desktop bindings"
                } else {
                    "Native desktop"
                };
                row().child(ui.picker("Shortcuts", mode, Action::Toggle(Setting::DesktopBindings)))
            })
            .child(div().text_size(TEXT).child(format!(
                "Desktop bindings: assign {command} --toggle and {command} --cancel in your desktop's shortcut settings."
            )))
    }

    fn microphone_name(&self) -> SharedString {
        match &self.config.microphone {
            None => "System default".into(),
            Some(id) => match self.microphones.iter().find(|device| device.id == *id) {
                Some(device) => device.name.clone().into(),
                None if self.microphone_listing.is_some() => "Checking microphones…".into(),
                None => "Unavailable · Choose another".into(),
            },
        }
    }

    fn microphone_section(&self, ui: &Ui<'_>) -> impl IntoElement {
        ui.section("Microphone").child(
            row()
                .child(ui.picker("Input device", self.microphone_name(), Action::Microphone))
                .child(ui.button("Refresh", Action::Refresh)),
        )
    }

    fn preferences_section(&self, ui: &Ui<'_>) -> impl IntoElement {
        ui.section("Preferences")
            .child(
                row()
                    .child(self.delivery_control(ui))
                    .child(self.switch(ui, "Reduce motion", Setting::ReduceMotion))
                    .child(self.switch(ui, "Play sounds", Setting::PlaySounds)),
            )
            .child(
                row()
                    .child(self.switch(ui, "Remove um / uh", Setting::RemoveFillers))
                    .child(ui.picker("Theme", self.config.theme.name(), Action::Theme)),
            )
            .child(
                div()
                    .text_size(TEXT)
                    .text_color(rgb(ui.palette.muted))
                    .child(if !self.config.remove_fillers {
                        "Filler removal is off."
                    } else if self.config.fillers(&self.config.language)
                        == crate::transcript::Fillers::Remove
                    {
                        "Standalone English um / uh are removed."
                    } else if self.config.language.eq_ignore_ascii_case("auto") {
                        "Automatic preserves words. Select English to remove um / uh."
                    } else {
                        "This language preserves words. Select English to remove um / uh."
                    }),
            )
            .when(
                cfg!(target_os = "windows") && ui.cx.global::<Services>().default_settings,
                |section| section.child(self.start_at_login(ui)),
            )
    }

    fn start_at_login(&self, ui: &Ui<'_>) -> impl IntoElement {
        row()
            .items_center()
            .child(self.switch(ui, "Start at login", Setting::StartAtLogin))
            .when(self.config.start_at_login && self.startup_apps_off, |row| {
                row.child(
                    div()
                        .flex_1()
                        .text_size(TEXT)
                        .text_color(rgb(ui.palette.warn))
                        .child("Turned off in Windows' Startup apps. Turn it on there."),
                )
            })
    }

    fn switch(&self, ui: &Ui<'_>, label: &'static str, setting: Setting) -> impl IntoElement {
        ui.toggle(label, setting.is_on(&self.config), Action::Toggle(setting))
    }

    #[cfg(not(target_os = "linux"))]
    fn delivery_control(&self, ui: &Ui<'_>) -> impl IntoElement {
        ui.toggle(
            "Keep clipboard",
            matches!(self.config.text_delivery, TextDelivery::Type),
            Action::Delivery,
        )
    }

    #[cfg(target_os = "linux")]
    fn delivery_control(&self, ui: &Ui<'_>) -> impl IntoElement {
        let method = match self.config.text_delivery {
            TextDelivery::Paste => "Paste (Ctrl+V)",
            TextDelivery::TerminalPaste => "Paste (Ctrl+Shift+V)",
            TextDelivery::Type => "Type (Keep clipboard)",
            TextDelivery::Copy => "Copy for manual paste",
        };
        ui.picker("Insert", method, Action::Delivery)
    }

    fn language_control(&self, ui: &Ui<'_>) -> AnyElement {
        let cleanup = self.config.engine == Engine::Nemo && !self.config.uses_nemotron();
        let languages = language::options(&self.config);
        let code = language::selected(&self.config);
        let name = languages
            .iter()
            .find_map(|&(language, name)| (language == code).then_some(name))
            .unwrap_or(code);
        let eyebrow = if cleanup {
            "Cleanup language"
        } else {
            "Language"
        };
        div()
            .id("language-picker")
            .track_focus(&self.language.group)
            .on_key_down(ui.cx.listener(Self::language_key))
            .on_key_up(ui.cx.listener(Self::language_release))
            .flex()
            .flex_col()
            .items_start()
            .gap(ROW_GAP)
            .child(
                ui.picker(eyebrow, name.to_owned(), Action::Language)
                    .track_focus(&self.language.button)
                    .on_click(|_, _, cx| cx.stop_propagation()),
            )
            .when(cleanup, |column| {
                column.child(
                    div()
                        .text_size(TEXT)
                        .text_color(rgb(ui.palette.muted))
                        .child("Parakeet detects speech automatically. This choice only controls English filler removal."),
                )
            })
            .when(self.language.cursor.is_some(), |column| {
                column.child(
                    div()
                        .id("language-choices")
                        .role(gpui::Role::ListBox)
                        .aria_label("Languages")
                        .max_h(px(200.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .track_scroll(&self.language.scroll)
                        .track_focus(&self.language.menu)
                        .children(languages.iter().enumerate().map(
                            |(index, &(language, name))| {
                                ui.kit().option(
                                    SharedString::from(format!("language-option-{language}")),
                                    name,
                                    language == code,
                                    self.language.cursor == Some(index),
                                    ui.press(Action::SelectLanguage(language)),
                                )
                            },
                        )),
                )
            })
            .into_any_element()
    }

    fn commands(&self, ui: &Ui<'_>, unsaved: bool) -> impl IntoElement {
        let services = ui.cx.global::<Services>();
        let running = services.is_running();
        let primary = if self.is_setting_up() && self.config.needs_engine() {
            None
        } else {
            primary_command(is_demo(ui.cx), services.service_state(), unsaved)
        };
        row()
            .when_some(primary, |row, (label, action)| {
                row.child(ui.command(label, action, Weight::Primary))
            })
            .when(
                unsaved && self.setup.is_none() && self.saving.is_none(),
                |row| row.child(ui.button("Revert", Action::Revert)),
            )
            .when(running, |row| row.child(ui.button("Pause", Action::Pause)))
            .child(ui.button("Shows…", Action::Shows))
            .child(div().flex_1())
            .child(ui.command("Quit", Action::Quit, Weight::Quiet))
    }

    fn status_line(&self, palette: &'static Palette, unsaved: bool) -> impl IntoElement {
        let tone = |problem| rgb(if problem { palette.warn } else { palette.muted });
        let attention = self.presentation.indicator == Indicator::Attention
            || self.notice.as_ref().is_some_and(super::Notice::is_problem);
        let marker = if attention {
            palette.warn
        } else if unsaved {
            palette.lamp
        } else {
            palette.muted
        };
        let lines = div()
            .flex_1()
            .min_w(px(0.0))
            .child(
                div()
                    .text_color(tone(self.presentation.indicator == Indicator::Attention))
                    .child(self.presentation.message.clone()),
            )
            .when_some(self.notice.as_ref(), |status, notice| {
                status.child(
                    div()
                        .text_color(tone(notice.is_problem()))
                        .child(notice.text()),
                )
            })
            .when(unsaved, |status| {
                status.child(div().text_color(tone(false)).child("Unsaved changes"))
            });
        row()
            .gap(px(10.0))
            .text_size(TEXT)
            .line_height(STATUS_LINE)
            .child(
                div()
                    .flex_none()
                    .h(STATUS_LINE)
                    .flex()
                    .items_center()
                    .child(facet(marker, 8.0)),
            )
            .child(lines)
    }
}

impl Ui<'_> {
    fn kit(&self) -> Kit<'static> {
        Kit::new(self.palette)
    }

    fn press(&self, action: Action) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        self.cx
            .listener(move |view, _, window, cx| view.act(action, window, cx))
    }

    fn command(
        &self,
        label: impl Into<SharedString>,
        action: Action,
        weight: Weight,
    ) -> gpui::Stateful<Div> {
        self.kit()
            .button(action.element_id(), label, weight, Some(self.press(action)))
    }

    fn button(&self, label: impl Into<SharedString>, action: Action) -> gpui::Stateful<Div> {
        self.command(label, action, Weight::Secondary)
    }

    fn picker(
        &self,
        eyebrow: &'static str,
        value: impl Into<SharedString>,
        action: Action,
    ) -> gpui::Stateful<Div> {
        self.kit().picker(
            action.element_id(),
            eyebrow,
            value,
            Some(self.press(action)),
        )
    }

    fn toggle(&self, label: &'static str, on: bool, action: Action) -> gpui::Stateful<Div> {
        let state = if on { "On" } else { "Off" };
        self.kit().toggle(
            action.element_id(),
            label,
            format!("{label}: {state}"),
            on,
            Some(self.press(action)),
        )
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

    fn section(&self, title: &'static str) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(ROW_GAP)
            .child(self.kit().eyebrow(title))
    }
}

fn row() -> Div {
    div().flex().gap(ROW_GAP)
}

fn primary_command(
    demo: bool,
    service: ServiceState,
    unsaved: bool,
) -> Option<(&'static str, Action)> {
    if demo {
        return Some(("Replay preview", Action::Preview));
    }
    match service {
        ServiceState::Running => Some(("Save changes", Action::Save)),
        ServiceState::NotSetUp => Some(("Enable dictation", Action::Save)),
        ServiceState::Paused | ServiceState::Pausing if unsaved => {
            Some(("Save and resume", Action::Save))
        },
        ServiceState::Paused => Some(("Resume dictation", Action::Resume)),
        ServiceState::Pausing | ServiceState::Quitting => None,
    }
}

fn setup_detail(progress: Option<Progress>) -> String {
    const MACHINE: &str = if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "PC"
    };
    let Some(Progress { step, done, total }) = progress else {
        return format!(
            "Speakeasy downloads the Parakeet speech model and the engine that suits this {MACHINE}, about 0.8 GB, then turns dictation on."
        );
    };
    if total > 0 {
        format!("{step} · {} of {} MB", done / 1_000_000, total / 1_000_000)
    } else {
        format!("{step}…")
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

fn header(indicator: Indicator, palette: &'static Palette, compact: bool) -> impl IntoElement {
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
        .child(Kit::new(palette).masthead(indicator_color))
        .when(!compact, |header| {
            header
                .child(
                    div()
                        .text_size(px(14.0))
                        .text_color(rgb(palette.muted))
                        .child("Say the word."),
                )
                .child(Kit::new(palette).keystone_rule())
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_dictation_resumes_and_only_unsaved_edits_save_first() {
        let command = |service, unsaved| primary_command(false, service, unsaved);
        assert_eq!(
            command(ServiceState::Paused, false),
            Some(("Resume dictation", Action::Resume))
        );
        assert_eq!(
            command(ServiceState::Paused, true),
            Some(("Save and resume", Action::Save))
        );
        assert_eq!(
            command(ServiceState::Pausing, true),
            Some(("Save and resume", Action::Save))
        );
        assert_eq!(command(ServiceState::Pausing, false), None);
        assert_eq!(
            command(ServiceState::Running, false),
            Some(("Save changes", Action::Save))
        );
        assert_eq!(
            command(ServiceState::NotSetUp, true),
            Some(("Enable dictation", Action::Save))
        );
    }
}
