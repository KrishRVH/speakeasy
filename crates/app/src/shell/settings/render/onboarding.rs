use gpui::{AnyElement, Div, FontWeight, SharedString, div, prelude::*, px, rgb};

use super::{Action, Settings, Ui, cycle_focus, dictation::level, fraction, progress_text};
use crate::{
    controls::{Tone, Weight, icon},
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
        let (label, action, enabled) = self.onboarding_primary(step, ui);
        let back = match step {
            Step::Prepare if self.setup.is_some() => {
                Some(ui.button("Cancel download", Action::Setup, Weight::Quiet))
            },
            Step::Prepare => None,
            _ => Some(ui.button("Back", Action::Onboarding(TourAction::Back), Weight::Quiet)),
        };
        let note = if is_demo(ui.cx) {
            Some((
                SharedString::from("Preview only. Simulated speech, no microphone."),
                false,
            ))
        } else {
            self.notice
                .as_ref()
                .map(|notice| (notice.text(), notice.is_problem()))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .on_key_down(cycle_focus)
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
                            .when_some(note, |status, (text, problem)| {
                                status
                                    .aria_label(text.clone())
                                    .text_color(rgb(if problem {
                                        palette.warn
                                    } else {
                                        palette.muted
                                    }))
                                    .child(text)
                            }),
                    )
                    .child(ui.button_if(
                        label,
                        action,
                        Weight::Primary,
                        enabled && self.onboarding_completion.is_none(),
                    )),
            )
    }

    fn onboarding_primary(&self, step: Step, ui: &Ui<'_>) -> (SharedString, Action, bool) {
        let running = is_demo(ui.cx) || ui.cx.global::<Services>().is_running();
        let next = Action::Onboarding(TourAction::Next);
        let (label, action, enabled) = match step {
            Step::Prepare if self.unreadable || self.resetting() => ("Continue", next, false),
            Step::Prepare if self.setup.is_some() => ("Setting up…", next, false),
            Step::Prepare if self.retiring_setup.is_some() => ("Pausing download…", next, false),
            Step::Prepare | Step::Practice if self.saving.is_some() => ("Saving…", next, false),
            Step::Prepare if !is_demo(ui.cx) && self.config.needs_engine() => {
                ("Set up automatically · 0.8 GB", Action::Setup, true)
            },
            Step::Permissions | Step::Practice if !running => {
                ("Retry dictation", Action::Resume, true)
            },
            Step::Practice
                if self.practice.text.trim().is_empty() && self.practice.task.is_none() =>
            {
                (
                    if is_demo(ui.cx) {
                        "Simulate dictation"
                    } else {
                        "Start practice"
                    },
                    Action::Practice,
                    true,
                )
            },
            Step::Ready => (
                "Start using Speakeasy",
                Action::Onboarding(TourAction::Finish),
                true,
            ),
            _ => (
                if step == Step::Permissions {
                    "Try dictation"
                } else {
                    "Continue"
                },
                next,
                self.onboarding_next(step, ui.cx) != step,
            ),
        };
        (label.into(), action, enabled)
    }

    fn onboarding_prepare(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let blocked = self.unreadable || self.resetting();
        let installing = self
            .setup
            .as_ref()
            .map(|installing| installing.setup.progress());
        let needs_setup =
            !blocked && !is_demo(ui.cx) && (self.config.needs_engine() || installing.is_some());
        let state: AnyElement = if blocked {
            kit.banner(
                Tone::Warn,
                "Settings can't be read",
                super::unreadable_text(ui.cx),
                Some(self.reset_controls(ui, "Start fresh…")),
            )
            .into_any_element()
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
            .into_any_element()
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
            .into_any_element()
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
        let help = |label: &'static str, help: permissions::Help| {
            ui.button(label, Action::PermissionHelp(help), Weight::Secondary)
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
                    help("Open settings", permissions::Help::Microphone),
                )
                .into_any_element(),
                kit.setting(
                    format!(
                        "Accessibility · {}",
                        permissions::description(observed.accessibility)
                    ),
                    "Lets your shortcut work and your words appear. If macOS asks, quit and reopen Speakeasy.",
                    help("Open settings", permissions::Help::Accessibility),
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
                    help(permissions::OPEN, permissions::Help::Microphone),
                )
                .into_any_element(),
            ]));
        page
    }

    fn onboarding_practice(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let active = self.practice.task.is_some();
        let recognized = !self.practice.text.trim().is_empty();
        let recording = active && self.presentation.indicator == Indicator::Recording;
        let external = cfg!(target_os = "linux") && self.config.linux.external_shortcut;
        let status = if recording {
            "Listening…"
        } else if active && self.presentation.indicator == Indicator::Busy {
            "Turning speech into text…"
        } else if recognized {
            "That worked. Continue when you're ready."
        } else if self.practice.ready || is_demo(ui.cx) {
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
            .child(
                div()
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
                            .when(recording, |status| {
                                status.child(level(palette.live, self.practice.level))
                            })
                            .child(status),
                    ),
            )
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
                    "Stop practice",
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
        let external = cfg!(target_os = "linux") && self.config.linux.external_shortcut;
        let cancel = self.presentation.cancel_shortcut.replace("Escape", "Esc");
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
        let shortcut = if external {
            div().child("Your desktop shortcut")
        } else {
            kit.keys(&self.shortcut(), false)
        };
        let mut lines = vec![line(
            shortcut,
            if external {
                "Start or finish"
            } else {
                "Hold to talk"
            },
        )];
        if !external {
            lines.push(line(
                div().child("Double-tap"),
                "Hands-free; press again to finish",
            ));
        }
        lines.push(line(
            if external {
                div().child("Your desktop's cancel binding")
            } else {
                kit.keys(&cancel, false)
            },
            "Cancel without typing",
        ));
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
            .child(ui.text("Come back any time: Settings → General → Setup guide."))
    }
}
