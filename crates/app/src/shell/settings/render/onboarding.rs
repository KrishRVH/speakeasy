use gpui::{
    InteractiveElement as _, SharedString, StatefulInteractiveElement as _,
    prelude::FluentBuilder as _,
};

use super::{
    Action, Div, EDGE, IntoElement as _, ParentElement as _, Settings, Styled as _, TEXT, Ui,
    cycle_focus, div, header, is_demo, permissions, progress_bar, px, relative, rgb, row,
    setup_detail,
};
use crate::{
    shell::{
        Services,
        settings::onboarding::{Action as TourAction, Step},
    },
    status::Indicator,
    theme::alpha,
};

impl Settings {
    pub(super) fn onboarding_view(&self, step: Step, ui: &Ui<'_>) -> Div {
        let page = match step {
            Step::Prepare => self.onboarding_prepare(ui),
            Step::Permissions => self.onboarding_permissions(ui),
            Step::Practice => self.onboarding_practice(ui),
            Step::Ready => self.onboarding_ready(ui),
        };
        let (label, action, enabled) = self.onboarding_primary(step, ui);
        div()
            .size_full()
            .bg(rgb(ui.palette.room))
            .text_color(rgb(ui.palette.ink))
            .flex()
            .flex_col()
            .on_key_down(cycle_focus)
            .child(
                div()
                    .p(EDGE)
                    .pb(px(20.0))
                    .min_h(px(88.0))
                    .flex_none()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(header(
                        if is_demo(ui.cx) {
                            Indicator::Ready
                        } else {
                            self.presentation.indicator
                        },
                        ui.palette,
                        true,
                    ))
                    .when(step != Step::Ready, |top| {
                        top.child(
                            ui.quiet_button("Skip setup", Action::Onboarding(TourAction::Finish)),
                        )
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
                    .px(EDGE)
                    .pb(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(20.0))
                    .child(
                        div()
                            .text_size(TEXT)
                            .text_color(rgb(ui.palette.muted))
                            .child(format!(
                                "{} · {}",
                                if is_demo(ui.cx) {
                                    "SETUP PREVIEW"
                                } else {
                                    "WELCOME TO SPEAKEASY"
                                },
                                step.position()
                            )),
                    )
                    .child(div().text_size(px(28.0)).child(step.heading()))
                    .child(page),
            )
            .child(
                div()
                    .px(EDGE)
                    .py(px(20.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        row()
                            .justify_between()
                            .items_center()
                            .child(
                                row()
                                    .when(step != Step::Prepare, |row| {
                                        row.child(ui.quiet_button(
                                            "Back",
                                            Action::Onboarding(TourAction::Back),
                                        ))
                                    })
                                    .when(step == Step::Prepare && self.is_setting_up(), |row| {
                                        row.child(ui.quiet_button("Cancel", Action::Setup))
                                    }),
                            )
                            .child(ui.primary_button(
                                label,
                                action,
                                enabled && self.onboarding_completion.is_none(),
                            )),
                    )
                    .child(div().min_h(px(36.0)).child(if is_demo(ui.cx) {
                        div()
                            .text_size(TEXT)
                            .text_color(rgb(ui.palette.muted))
                            .child("Preview only · Simulated speech · No microphone access")
                            .into_any_element()
                    } else {
                        self.status_line(ui.palette, self.unsaved(ui.cx))
                            .into_any_element()
                    })),
            )
    }

    fn onboarding_primary(&self, step: Step, ui: &Ui<'_>) -> (SharedString, Action, bool) {
        let running = is_demo(ui.cx) || ui.cx.global::<Services>().is_running();
        let next = Action::Onboarding(TourAction::Next);
        let (label, action, enabled) = match step {
            Step::Prepare if self.is_setting_up() => ("Setting up…", next, false),
            Step::Prepare if self.retiring_setup.is_some() => ("Pausing download…", next, false),
            Step::Prepare | Step::Practice if self.saving.is_some() => ("Saving…", next, false),
            Step::Prepare if !is_demo(ui.cx) && self.config.needs_engine() => {
                ("Set up automatically · 0.8 GB", Action::Setup, true)
            },
            Step::Prepare | Step::Practice if self.unsaved(ui.cx) => {
                ("Save and continue", Action::Save, true)
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
        let needs_setup = !is_demo(ui.cx) && (self.config.needs_engine() || self.is_setting_up());
        let progress = self.setup.as_ref().map(|(setup, _, _)| setup.progress());
        div().flex().flex_col().gap(px(20.0)).text_size(px(15.0))
            .child(div().text_color(rgb(ui.palette.lamp)).child("Say the word."))
            .child("Your voice becomes text on this computer. No account, no dictation history, no recording uploads.")
            .child(ui.panel().flex().flex_col().gap(px(12.0))
                .child(if needs_setup { setup_detail(progress) } else { "Local speech is installed. Let's try your shortcut.".into() })
                .when_some(progress, |card, progress| card.child(progress_bar(progress, ui.palette))))
            .when(needs_setup, |page| page.child(div().text_size(TEXT).text_color(rgb(ui.palette.muted))
                .child("One download, then you can dictate offline. Skip returns to Settings without stopping an active download.")))
    }

    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::unused_self,
            reason = "Only macOS provides passive permission observations"
        )
    )]
    fn onboarding_permissions(&self, ui: &Ui<'_>) -> Div {
        let page = div().flex().flex_col().gap(px(20.0)).text_size(px(15.0));
        #[cfg(target_os = "macos")]
        let page = {
            let observed = self.permissions.unwrap_or_default();
            page.child("Let Speakeasy hear you and type for you.")
                .child(ui.panel().flex().flex_col().gap(px(12.0))
                    .child(format!("Microphone: {}", permissions::description(observed.microphone)))
                    .child("Your first recording asks for microphone access. If you previously denied it, enable Speakeasy in System Settings.")
                    .child(row().child(ui.quiet_button("Microphone settings", Action::PermissionHelp(permissions::Help::Microphone)))))
                .child(ui.panel().flex().flex_col().gap(px(12.0))
                    .child(format!("Accessibility: {}", permissions::description(observed.accessibility)))
                    .child("Enable Speakeasy to use your shortcut and insert text. If macOS asks for a restart, quit and reopen Speakeasy.")
                    .child(row().child(ui.quiet_button("Accessibility settings", Action::PermissionHelp(permissions::Help::Accessibility)))))
        };
        #[cfg(not(target_os = "macos"))]
        let page = page.child("Next, try a sentence in this window. Nothing records until you use your shortcut.")
            .child(ui.panel().flex().flex_col().gap(px(12.0))
                .child(if cfg!(target_os = "windows") {
                    "If Windows blocks the microphone, enable microphone access for desktop apps."
                } else {
                    "If your desktop asks to allow the dictation shortcut, approve it. You can check your microphone in the next step."
                })
                .child(row().child(ui.quiet_button(if cfg!(target_os = "windows") { "Microphone settings" } else { "Microphone help" }, Action::PermissionHelp(permissions::Help::Microphone)))));
        page
    }

    fn onboarding_practice(&self, ui: &Ui<'_>) -> Div {
        let active = self.practice.task.is_some();
        let recognized = !self.practice.text.trim().is_empty();
        let recording = active && self.presentation.indicator == Indicator::Recording;
        let status = if recording {
            "Listening…"
        } else if active && self.presentation.indicator == Indicator::Busy {
            "Turning speech into text…"
        } else if recognized {
            "Got it"
        } else if self.practice.ready || is_demo(ui.cx) {
            "Ready when you are"
        } else if active {
            "Preparing private practice…"
        } else {
            "Practice paused"
        };
        div().flex().flex_col().gap(px(12.0)).text_size(px(15.0))
            .child(ui.panel().flex().flex_col().gap(px(10.0))
                .when(recognized, |card| card.border_color(rgb(ui.palette.lamp)))
                .child(div().text_size(px(22.0)).child(self.presentation.shortcut.to_string()))
                .child(div().text_size(TEXT).text_color(rgb(ui.palette.muted)).child(
                    if cfg!(target_os = "linux") && self.config.linux.external_shortcut { "Press, say a sentence, then press again." } else { "Hold, say a sentence, release." }))
                .child(div().text_size(TEXT).text_color(rgb(if recognized { ui.palette.lamp } else { ui.palette.muted })).child(status))
                .child(div().h(px(4.0)).w_full().bg(rgb(ui.palette.raise))
                    .child(div().h_full().w(relative(if recording { self.practice.level } else { 0.0 })).bg(rgb(ui.palette.live))))
                .child(div().id("onboarding-transcript").min_h(px(48.0)).max_h(px(110.0)).overflow_y_scroll().text_size(px(18.0))
                    .child(if recognized {
                        div().child(gpui::text!(id = "onboarding-words", self.practice.text.to_string())).into_any_element()
                    } else { div().text_color(rgb(ui.palette.muted)).child("Your words appear here.").into_any_element() }))
                .when(recognized, |card| card.child(div().text_size(TEXT).text_color(rgb(ui.palette.lamp)).child("That worked. Continue when you're ready.")))
                .when(active, |card| card.child(row().child(ui.quiet_button("Stop", Action::Practice)))))
            .child(row().items_center().gap(px(32.0))
                .child(ui.quiet_button(format!("Microphone: {}", self.microphone_name()), Action::Microphone).flex_1().max_w_full().min_w(px(0.0)))
                .child(ui.quiet_button("Refresh", Action::Refresh)))
            .child(div().text_size(TEXT).text_color(rgb(ui.palette.muted)).child(if is_demo(ui.cx) {
                "Demo words only. Your microphone is off."
            } else { "Words stay here, never pasted or saved. Leaving this window clears practice." }))
            .child(row().child(ui.quiet_button("Not hearing anything? Microphone help", Action::PermissionHelp(permissions::Help::Microphone))))
    }

    fn onboarding_ready(&self, ui: &Ui<'_>) -> Div {
        let external = cfg!(target_os = "linux") && self.config.linux.external_shortcut;
        div().flex().flex_col().gap(px(20.0)).text_size(px(15.0))
            .child(if is_demo(ui.cx) { "Preview complete. Real dictation stays on this computer, too." } else { "That was all on this computer." })
            .child(ui.panel().flex().flex_col().gap(px(16.0))
                .child(ui.shortcut_row(self.presentation.shortcut.to_string(), if external { "start or finish" } else { "hold to speak" }))
                .when(!external, |card| card.child(ui.shortcut_row("Double-tap", "hands-free; press again to finish")))
                .child(ui.shortcut_row(self.presentation.cancel_shortcut.to_string(), "cancel without typing")))
            .child("Practice kept your words here. Now try your shortcut in a note or a chat to see them arrive in another app.")
            .child(div().text_size(TEXT).text_color(rgb(ui.palette.muted)).child("Come back any time: Settings → Advanced → Replay setup."))
    }
}

impl Ui<'_> {
    fn primary_button(
        &self,
        label: impl Into<SharedString>,
        action: Action,
        enabled: bool,
    ) -> gpui::Stateful<Div> {
        self.button_enabled(label, action, enabled)
            .text_size(px(13.0))
            .px(px(16.0))
            .py(px(10.0))
            .bg(rgb(self.palette.panel))
            .border_color(rgb(if enabled {
                self.palette.lamp
            } else {
                self.palette.raise
            }))
            .text_color(rgb(if enabled {
                self.palette.ink
            } else {
                self.palette.muted
            }))
    }

    fn quiet_button(&self, label: impl Into<SharedString>, action: Action) -> gpui::Stateful<Div> {
        self.button(label, action)
            .mx(px(-12.0))
            .bg(alpha(self.palette.room, 0.0))
            .border_color(alpha(self.palette.room, 0.0))
            .text_color(rgb(self.palette.muted))
    }

    fn shortcut_row(&self, keys: impl Into<SharedString>, meaning: &'static str) -> Div {
        row()
            .flex_wrap()
            .justify_between()
            .child(div().child(keys.into()))
            .child(div().text_color(rgb(self.palette.muted)).child(meaning))
    }
}
