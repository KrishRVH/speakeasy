use gpui::{Div, FontWeight, div, prelude::*, px, rgb};

use super::{Action, Notice, Settings, Ui, dictation::level, fraction, progress_text};
use crate::{
    controls::{Weight, icon},
    icons,
    shell::{
        Services,
        settings::{
            is_demo,
            menu::Menu,
            onboarding::{Action as TourAction, Step},
            permissions,
        },
    },
    status::Indicator,
};

impl Settings {
    pub(super) fn onboarding_view(&self, step: Step, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let page = match step {
            Step::Prepare => self.onboarding_prepare(ui),
            Step::Permissions => self.onboarding_permissions(ui),
            Step::Practice => self.onboarding_practice(ui),
            Step::Ready => self.onboarding_ready(ui),
        };
        let (label, action) = self.onboarding_primary(step, ui);
        let back = match step {
            Step::Prepare if self.engine_work.installing().is_some() => {
                Some(ui.button("Cancel download", Action::Setup, Weight::Quiet))
            },
            Step::Prepare => None,
            _ => Some(ui.button("Back", Action::Onboarding(TourAction::Back), Weight::Quiet)),
        };
        let preview = is_demo(ui.cx)
            .then(|| Notice::Note("Preview only. Simulated speech, no microphone.".into()));
        let note = preview.as_ref().or(self.notice.as_ref());
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .min_h(px(56.0))
                    .px(px(24.0))
                    .pt(px(12.0))
                    .child(kit.detail(format!(
                        "{} · Step {}",
                        if is_demo(ui.cx) {
                            "Setup preview"
                        } else {
                            "Welcome to Speakeasy"
                        },
                        step.position()
                    )))
                    .when(step != Step::Ready, |top| {
                        top.child(ui.button(
                            "Skip setup",
                            Action::Onboarding(TourAction::Finish),
                            Weight::Quiet,
                        ))
                    }),
            )
            .child(
                div()
                    .id("onboarding")
                    .track_focus(&self.onboarding_focus)
                    .track_scroll(&self.navigation.scroll)
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .px(px(24.0))
                    .pt(px(8.0))
                    .pb(px(24.0))
                    .flex()
                    .flex_col()
                    .gap(px(20.0))
                    .child(
                        div()
                            .text_size(px(24.0))
                            .line_height(px(30.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(step.heading()),
                    )
                    .child(page),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .min_h(px(60.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .border_t_1()
                    .border_color(palette.line())
                    .bg(rgb(palette.door))
                    .children(back)
                    .child(
                        div()
                            .id("onboarding-status")
                            .role(gpui::Role::Status)
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .when_some(note, |status, note| {
                                status
                                    .aria_label(note.text())
                                    .text_color(rgb(if note.is_problem() {
                                        palette.warn
                                    } else {
                                        palette.muted
                                    }))
                                    .child(note.text())
                            }),
                    )
                    .child(
                        kit.button(
                            "onboarding-primary",
                            label,
                            Weight::Primary,
                            action
                                .filter(|_| self.onboarding_completion.is_none())
                                .map(|action| ui.press(action)),
                        ),
                    ),
            )
    }

    fn onboarding_primary(&self, step: Step, ui: &Ui<'_>) -> (&'static str, Option<Action>) {
        let demo = is_demo(ui.cx);
        let running = demo || ui.cx.global::<Services>().is_running();
        let advance = || {
            (self.onboarding_next(step, ui.cx) != step)
                .then_some(Action::Onboarding(TourAction::Next))
        };
        match step {
            Step::Prepare if self.locked() => ("Continue", None),
            Step::Prepare if self.engine_work.installing().is_some() => ("Setting up…", None),
            Step::Prepare if self.engine_work.is_pausing() => ("Pausing download…", None),
            Step::Prepare | Step::Practice if self.saving.is_some() => ("Saving…", None),
            Step::Prepare if !demo && self.config.needs_engine() => (
                "Set up automatically · 0.8 GB",
                (!self.engine_work.is_busy()).then_some(Action::Setup),
            ),
            Step::Permissions | Step::Practice if !running => {
                ("Retry dictation", Some(Action::Resume))
            },
            Step::Practice if !self.practice.recognized() && self.practice.live.is_none() => (
                if demo {
                    "Simulate dictation"
                } else {
                    "Start practice"
                },
                Some(Action::Practice),
            ),
            Step::Ready => (
                "Start using Speakeasy",
                Some(Action::Onboarding(TourAction::Finish)),
            ),
            Step::Permissions => ("Try dictation", advance()),
            Step::Prepare | Step::Practice => ("Continue", advance()),
        }
    }

    fn onboarding_prepare(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let installing = self
            .engine_work
            .installing()
            .map(|installing| installing.setup.progress());
        let needs_setup = !self.locked()
            && !is_demo(ui.cx)
            && (self.config.needs_engine() || installing.is_some());
        let state = if self.locked() {
            self.unreadable_banner(ui)
        } else if needs_setup {
            kit.group(vec![
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .p(px(16.0))
                    .child(kit.label("Parakeet speech model", super::setup_detail()))
                    .when_some(installing, |row, progress| {
                        row.child(kit.progress(fraction(progress)))
                            .child(kit.detail(progress_text(progress)))
                    })
                    .into_any_element(),
            ])
        } else {
            kit.group(vec![
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .p(px(16.0))
                    .child(icon(icons::CHECK, 16.0, ui.palette.lamp))
                    .child(kit.label("Local speech is ready", "Next, try your shortcut."))
                    .into_any_element(),
            ])
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(div().text_size(px(15.0)).line_height(px(22.0)).child(
                "Your voice becomes text on this computer. No account, no dictation history, no uploads.",
            ))
            .child(state)
            .when(needs_setup, |page| {
                page.child(ui.text(
                    "One download, then you can dictate offline. Skip returns to Settings without stopping an active download.",
                ))
            })
    }

    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::unused_self,
            reason = "Only macOS provides passive permission observations"
        )
    )]
    fn onboarding_permissions(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let help = |topic: permissions::Help| {
            ui.button(
                permissions::OPEN,
                Action::PermissionHelp(topic),
                Weight::Secondary,
            )
        };
        let page = div().flex().flex_col().gap(px(16.0));
        #[cfg(target_os = "macos")]
        let page = {
            let observed = self.permissions.unwrap_or_default();
            page.child(
                div()
                    .text_size(px(15.0))
                    .child("Let Speakeasy hear you and type for you."),
            )
            .child(kit.group(vec![
                kit.setting(
                    format!(
                        "Microphone · {}",
                        permissions::description(observed.microphone)
                    ),
                    "Your first recording asks for access. If you denied it before, turn on Speakeasy in System Settings.",
                    help(permissions::Help::Microphone),
                )
                .into_any_element(),
                kit.setting(
                    format!(
                        "Accessibility · {}",
                        permissions::description(observed.accessibility)
                    ),
                    "Lets your shortcut work and your words appear. If macOS asks, quit and reopen Speakeasy.",
                    help(permissions::Help::Accessibility),
                )
                .into_any_element(),
            ]))
        };
        #[cfg(not(target_os = "macos"))]
        let page = page
            .child(div().text_size(px(15.0)).child(
                "Next, try a sentence in this window. Nothing records until you use your shortcut.",
            ))
            .child(kit.group(vec![
                kit.setting(
                    "Microphone",
                    if cfg!(target_os = "windows") {
                        "If Windows blocks the microphone, turn on microphone access for desktop apps."
                    } else {
                        "If your desktop asks to allow the dictation shortcut, approve it."
                    },
                    help(permissions::Help::Microphone),
                )
                .into_any_element(),
            ]));
        page
    }

    fn onboarding_practice(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let live = self.practice.live.as_ref();
        let active = live.is_some();
        let recognized = self.practice.recognized();
        let recording = live.filter(|_| self.presentation.indicator == Indicator::Recording);
        let external = self.uses_desktop_bindings();
        let status = if recording.is_some() {
            "Listening…"
        } else if active && self.presentation.indicator == Indicator::Busy {
            "Transcribing locally…"
        } else if recognized {
            "That worked. Continue when you're ready."
        } else if live.is_some_and(|live| live.ready) || is_demo(ui.cx) {
            if external {
                "Press your shortcut, say a sentence, then press it again."
            } else {
                "Hold your shortcut, say a sentence, then release."
            }
        } else if active {
            "Preparing practice…"
        } else {
            "Practice is paused."
        };
        let words = if recognized {
            div()
                .text_size(px(18.0))
                .line_height(px(26.0))
                .child(gpui::text!(
                    id = "onboarding-words",
                    self.practice.text.to_string()
                ))
        } else {
            div()
                .text_size(px(15.0))
                .text_color(rgb(palette.muted))
                .child("Your words appear here.")
        };
        let heading = div()
            .flex()
            .items_center()
            .gap(px(16.0))
            .when(!external, |row| row.child(kit.keys(&self.shortcut(), true)))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .text_color(rgb(if recognized {
                        palette.lamp
                    } else {
                        palette.muted
                    }))
                    .when_some(recording, |status, live| {
                        status.child(level(palette.live, live.level))
                    })
                    .child(status),
            );
        #[cfg(not(target_os = "linux"))]
        let heading = heading.child(self.shortcut_choice(ui));
        let card = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(20.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(if recognized {
                rgb(palette.lamp)
            } else {
                palette.line()
            })
            .bg(rgb(palette.panel))
            .child(heading)
            .when(!cfg!(target_os = "linux"), |card| {
                card.child(kit.detail(self.shortcut_help()))
            })
            .child(
                div()
                    .id("onboarding-transcript")
                    .min_h(px(64.0))
                    .max_h(px(140.0))
                    .overflow_y_scroll()
                    .px(px(14.0))
                    .py(px(12.0))
                    .rounded(px(8.0))
                    .bg(rgb(palette.room))
                    .border_1()
                    .border_color(palette.line())
                    .child(words),
            )
            .when(active, |card| {
                card.child(div().flex().child(ui.button(
                    "End practice",
                    Action::Practice,
                    Weight::Quiet,
                )))
            });
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(card)
            .child(kit.group(vec![
                kit.setting(
                    "Microphone",
                    "Not hearing anything? Choose another input.",
                    self.dropdown(ui, Menu::Microphone, "Microphone", self.microphone_name()),
                )
                .into_any_element(),
            ]))
            .child(ui.text(if is_demo(ui.cx) {
                "Demo words only. Your microphone is off."
            } else {
                "Words stay here, never pasted or saved. Leaving this window clears practice."
            }))
    }

    fn onboarding_ready(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let external = self.uses_desktop_bindings();
        let line = |keys: Div, meaning: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(16.0))
                .px(px(16.0))
                .py(px(12.0))
                .child(div().flex_1().child(keys))
                .child(kit.detail(meaning))
                .into_any_element()
        };
        let lines = if external {
            vec![
                line(div().child("Your desktop shortcut"), "Start or finish"),
                line(
                    div().child("Your desktop's cancel binding"),
                    "Cancel without typing",
                ),
            ]
        } else {
            vec![
                line(kit.keys(&self.shortcut(), false), "Hold to talk"),
                line(
                    div().child("Double-tap"),
                    "Hands-free; press again to finish",
                ),
                line(
                    kit.keys(&self.cancel_shortcut(), false),
                    "Cancel without typing",
                ),
            ]
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(div().text_size(px(15.0)).child(if is_demo(ui.cx) {
                "Preview complete. Real dictation stays on this computer, too."
            } else {
                "That was all on this computer. Now try your shortcut in a note or a chat."
            }))
            .child(kit.group(lines))
            .child(ui.text("Come back any time: Settings › General › Setup guide."))
    }
}
