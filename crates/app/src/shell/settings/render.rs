mod dictation;
mod general;
mod onboarding;

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, KeyDownEvent, MouseDownEvent, SharedString,
    Stateful, Window, div, prelude::*, px, rgb,
};
use speakeasy_platform::Permission;

use super::{Action, Notice, Reset, Settings, is_demo, menu::Menu, navigation::Page, permissions};
use crate::{
    controls::{Kit, Tone, Weight, facet, hollow, icon, popover},
    icons,
    runtime::ModelState,
    setup::Progress,
    shell::Services,
    status::{Indicator, ServiceState},
    theme::Palette,
};

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.vocabulary.theme(self.config.theme, cx);
        self.recognition.theme(self.config.theme, cx);
        self.presets.theme(self.config.theme, cx);
        let palette = self.config.theme.palette();
        let ui = Ui {
            palette,
            keyboard: self.keyboard,
            cx,
        };
        let root = div()
            .size_full()
            .bg(rgb(palette.room))
            .text_color(rgb(palette.ink))
            .text_size(px(14.0))
            .flex()
            .flex_col()
            .on_key_down(cycle_focus)
            .capture_key_down(cx.listener(|view, _: &KeyDownEvent, _, cx| {
                if !view.keyboard {
                    view.keyboard = true;
                    cx.notify();
                }
            }))
            .capture_any_mouse_down(cx.listener(|view, _: &MouseDownEvent, _, cx| {
                if view.keyboard {
                    view.keyboard = false;
                    cx.notify();
                }
            }));
        if let Some(step) = self.onboarding {
            return root.child(self.onboarding_view(step, &ui));
        }
        root.when_some(self.banner(&ui), |root, banner| {
            root.child(div().flex_none().px(px(16.0)).pt(px(16.0)).child(banner))
        })
        .child(self.tabs(&ui))
        .child(
            div()
                .id("settings")
                .track_scroll(&self.navigation.scroll)
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .px(px(20.0))
                .pt(px(20.0))
                .pb(px(32.0))
                .child(div().flex().flex_col().gap(px(28.0)).children(
                    match self.navigation.page {
                        Page::Dictation => self.dictation_page(&ui),
                        Page::Words => self.words_page(&ui),
                        Page::General => self.general_page(&ui),
                    },
                )),
        )
        .child(self.footer(&ui))
    }
}

pub(super) struct Ui<'a> {
    pub(super) palette: &'static Palette,
    keyboard: bool,
    pub(super) cx: &'a Context<'a, Settings>,
}

impl Ui<'_> {
    pub(super) const fn kit(&self) -> Kit<'static> {
        Kit::new(self.palette).keyboard(self.keyboard)
    }

    pub(super) fn press(
        &self,
        action: Action,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        self.cx
            .listener(move |view, _, window, cx| view.act(action, window, cx))
    }

    pub(super) fn button(
        &self,
        label: impl Into<SharedString>,
        action: Action,
        weight: Weight,
    ) -> Stateful<Div> {
        self.button_if(label, action, weight, true)
    }

    pub(super) fn button_if(
        &self,
        label: impl Into<SharedString>,
        action: Action,
        weight: Weight,
        enabled: bool,
    ) -> Stateful<Div> {
        self.kit().button(
            action.element_id(),
            label,
            weight,
            enabled.then(|| self.press(action)),
        )
    }

    pub(super) fn switch(&self, label: &'static str, on: bool, action: Action) -> Stateful<Div> {
        self.switch_if(label, on, action, true)
    }

    pub(super) fn switch_if(
        &self,
        label: &'static str,
        on: bool,
        action: Action,
        enabled: bool,
    ) -> Stateful<Div> {
        self.kit().switch(
            action.element_id(),
            label,
            on,
            enabled.then(|| self.press(action)),
        )
    }

    pub(super) fn text(&self, text: impl Into<SharedString>) -> Div {
        self.kit().detail(text).px(px(2.0))
    }
}

impl Settings {
    fn tabs(&self, ui: &Ui<'_>) -> impl IntoElement {
        ui.kit()
            .tab_bar("settings-tabs", "Settings pages")
            .flex_none()
            .px(px(24.0))
            .pt(px(8.0))
            .children(Page::ALL.map(|page| {
                let action = Action::Page(page);
                ui.kit()
                    .tab(
                        action.element_id(),
                        page.label(),
                        self.navigation.page == page,
                        ui.press(action),
                    )
                    .track_focus(self.navigation.focus(page))
            }))
    }

    pub(super) fn dropdown(
        &self,
        ui: &Ui<'_>,
        menu: Menu,
        label: &'static str,
        value: impl Into<SharedString>,
    ) -> Div {
        let open = self.menus.open == Some(menu);
        let action = Action::Menu(menu);
        let trigger = ui
            .kit()
            .dropdown(action.element_id(), label, value, open, ui.press(action));
        self.anchor_menu(ui, menu, label, trigger, false)
    }

    pub(super) fn anchor_menu(
        &self,
        ui: &Ui<'_>,
        menu: Menu,
        label: &'static str,
        trigger: Stateful<Div>,
        align_right: bool,
    ) -> Div {
        let open = self.menus.open == Some(menu);
        div()
            .relative()
            .flex_none()
            .when(open, |anchor| {
                anchor
                    .track_focus(&self.menus.group)
                    .on_key_down(ui.cx.listener(Self::menu_key))
                    .on_key_up(ui.cx.listener(Self::menu_release))
            })
            .child(trigger.track_focus(self.menus.trigger(menu)))
            .when(open, |anchor| {
                anchor.child(popover(self.menu_list(ui, menu, label), align_right))
            })
    }

    fn menu_list(&self, ui: &Ui<'_>, menu: Menu, label: &'static str) -> impl IntoElement {
        let cursor = self.menus.cursor;
        ui.kit()
            .menu("menu-list", label)
            .when(matches!(menu, Menu::Preset(_)), |list| {
                list.role(gpui::Role::Menu)
            })
            .track_focus(&self.menus.list)
            .track_scroll(&self.menus.scroll)
            .children(
                self.menu_items(menu)
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let id = (SharedString::from("menu-item"), index);
                        let press = ui.press(item.action);
                        match item.chosen {
                            Some(chosen) => {
                                ui.kit()
                                    .option(id, item.label, chosen, index == cursor, press)
                            },
                            None => ui.kit().menu_action(
                                id,
                                item.label,
                                item.destructive,
                                index == cursor,
                                press,
                            ),
                        }
                    }),
            )
    }

    fn banner(&self, ui: &Ui<'_>) -> Option<Div> {
        let kit = ui.kit();
        if self.unreadable {
            return Some(kit.banner(
                Tone::Warn,
                "Settings can't be read",
                unreadable_text(ui.cx),
                Some(self.reset_controls(ui)),
            ));
        }
        if is_demo(ui.cx) {
            return Some(kit.banner(
                Tone::Lamp,
                "Preview",
                "Simulated audio. Nothing is recorded or inserted.",
                Some(div().child(ui.button("Replay preview", Action::Preview, Weight::Secondary))),
            ));
        }
        if self.config.needs_engine() && !self.own_engine && !self.resetting() {
            let progress = self
                .setup
                .as_ref()
                .map(|installing| installing.setup.progress());
            let controls = match progress {
                Some(progress) => div()
                    .flex_col()
                    .w_full()
                    .gap(px(8.0))
                    .child(kit.progress(fraction(progress)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(kit.detail(progress_text(progress)).flex_1())
                            .child(ui.button("Cancel", Action::Setup, Weight::Quiet)),
                    ),
                None => div().child(ui.button_if(
                    "Set up automatically",
                    Action::Setup,
                    Weight::Primary,
                    self.retiring_setup.is_none(),
                )),
            };
            return Some(kit.banner(
                Tone::Lamp,
                "Set up dictation",
                setup_detail(),
                Some(controls),
            ));
        }
        if self.microphone_blocked() {
            return Some(kit.banner(
                Tone::Warn,
                "Microphone access is off",
                microphone_remedy(),
                Some(div().child(ui.button(
                    permissions::OPEN,
                    Action::PermissionHelp(permissions::Help::Microphone),
                    Weight::Secondary,
                ))),
            ));
        }
        if self.presentation.model == Some(ModelState::Unavailable) {
            let model = self.config.model_choice();
            return Some(
                kit.banner(
                    Tone::Warn,
                    "Speech model unavailable",
                    format!(
                        "Speakeasy can't load {}. Download it again or choose another model.",
                        model.map_or("your model", crate::models::Model::label)
                    ),
                    Some(
                        div()
                            .when_some(model, |actions, model| {
                                actions.child(ui.button(
                                    "Repair",
                                    Action::Install(model),
                                    Weight::Secondary,
                                ))
                            })
                            .child(ui.button("Change model", Action::ChooseModel, Weight::Quiet)),
                    ),
                ),
            );
        }
        None
    }

    fn words_page(&self, ui: &Ui<'_>) -> Vec<AnyElement> {
        [
            Some(self.vocabulary_section(ui).into_any_element()),
            self.shows_hints()
                .then(|| self.recognition_section(ui).into_any_element()),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    fn microphone_blocked(&self) -> bool {
        self.permissions.is_some_and(|permissions| {
            matches!(
                permissions.microphone,
                Permission::NotGranted | Permission::Restricted
            )
        })
    }

    pub(super) fn reset_controls(&self, ui: &Ui<'_>) -> Div {
        let enabled = self.can_reset(ui.cx);
        let controls = div().flex().flex_wrap().items_center().gap(px(8.0));
        match self.reset {
            Reset::Idle => controls.child(ui.button_if(
                "Start fresh…",
                Action::Reset,
                Weight::Destroy,
                enabled,
            )),
            Reset::Confirming => controls
                .child(ui.button_if(
                    "Delete settings and downloads",
                    Action::ConfirmReset,
                    Weight::ConfirmDestroy,
                    enabled,
                ))
                .child(ui.button("Cancel", Action::CancelReset, Weight::Quiet)),
            Reset::Running { .. } => controls.child(ui.kit().detail(crate::status::RESETTING)),
        }
    }

    fn footer(&self, ui: &Ui<'_>) -> impl IntoElement {
        let palette = ui.palette;
        let problem = self.notice.as_ref().is_some_and(Notice::is_problem)
            || self.presentation.indicator == Indicator::Attention;
        let marker = match self.presentation.indicator {
            _ if problem => facet(palette.warn, 8.0),
            Indicator::Recording => facet(palette.live, 8.0),
            Indicator::Ready => facet(palette.lamp, 8.0),
            Indicator::Busy => hollow(palette.lamp, 8.0),
            Indicator::Attention | Indicator::Paused => hollow(palette.muted, 8.0),
        };
        let saved = self.saved.is_some() && self.notice.is_none();
        let message: SharedString = match &self.notice {
            Some(notice) => notice.text(),
            None if saved => "Saved".into(),
            None => self.presentation.message.as_str().into(),
        };
        let service = ui.cx.global::<Services>().service_state();
        let busy = self.unreadable || self.resetting();
        div()
            .flex_none()
            .min_h(px(52.0))
            .py(px(8.0))
            .pl(px(20.0))
            .pr(px(10.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .border_t_1()
            .border_color(palette.line())
            .bg(rgb(palette.door))
            .child(div().flex_none().mr(px(8.0)).child(marker))
            .child(
                div()
                    .id("status")
                    .role(gpui::Role::Status)
                    .aria_label(message.clone())
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .text_color(rgb(if problem { palette.warn } else { palette.ink }))
                    .child(div().min_w(px(0.0)).child(message))
                    .when(saved, |status| {
                        status.child(icon(icons::CHECK, 14.0, palette.lamp))
                    }),
            )
            .map(|footer| match service {
                ServiceState::Paused if !busy && !is_demo(ui.cx) => {
                    footer.child(ui.button("Resume", Action::Resume, Weight::Primary))
                },
                _ => footer.child(ui.button_if(
                    "Pause",
                    Action::Pause,
                    Weight::Quiet,
                    service == ServiceState::Running && !busy,
                )),
            })
            .child(ui.button("Shows", Action::Shows, Weight::Quiet))
            .child(ui.button("Quit", Action::Quit, Weight::Quiet))
    }
}

fn unreadable_text(cx: &App) -> String {
    let path = &cx.global::<Services>().path;
    let file = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    format!(
        "Speakeasy can't read {file}, often because another version wrote it. Fix the error and restart, or start fresh: Speakeasy removes its settings and downloads, then downloads Parakeet again. Your Shows stay."
    )
}

const fn setup_detail() -> &'static str {
    if cfg!(target_os = "macos") {
        "Speakeasy downloads the Parakeet speech model and the engine that suits this Mac, about 0.8 GB, then turns dictation on."
    } else {
        "Speakeasy downloads the Parakeet speech model and the engine that suits this PC, about 0.8 GB, then turns dictation on."
    }
}

const fn microphone_remedy() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows is blocking desktop apps from the microphone. Turn on microphone access for desktop apps, then come back here."
    } else if cfg!(target_os = "macos") {
        "Turn on Speakeasy in System Settings › Privacy & Security › Microphone, then come back here."
    } else {
        "Your desktop blocked the microphone. Allow it in your privacy settings, then come back here."
    }
}

pub(super) fn fraction(progress: Progress) -> f32 {
    progress.done as f32 / progress.total.max(1) as f32
}

pub(super) fn progress_text(progress: Progress) -> String {
    let Progress { step, done, total } = progress;
    if total > 0 {
        format!("{step} · {} of {} MB", done / 1_000_000, total / 1_000_000)
    } else {
        format!("{step}…")
    }
}

fn cycle_focus(event: &KeyDownEvent, window: &mut Window, cx: &mut App) {
    if event.keystroke.key == "tab" {
        if event.keystroke.modifiers.shift {
            window.focus_prev(cx);
        } else {
            window.focus_next(cx);
        }
        cx.stop_propagation();
    }
}
