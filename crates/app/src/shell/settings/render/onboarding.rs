use gpui::{InteractiveElement as _, StatefulInteractiveElement as _, prelude::FluentBuilder as _};

use super::{
    Action, Div, EDGE, IntoElement as _, ParentElement as _, Settings, Styled as _, TEXT, Ui,
    cycle_focus, div, header, is_demo, permissions, px, relative, rgb, row,
};
use crate::{
    shell::{
        Services,
        settings::onboarding::{Action as TourAction, Step},
    },
    status::Indicator,
};

impl Settings {
    pub(super) fn onboarding_view(&self, step: Step, ui: &Ui<'_>) -> Div {
        let advancing = self.onboarding_next(step, ui.cx) != step;
        let page = match step {
            Step::Prepare => self.onboarding_prepare(ui),
            Step::Permissions => self.onboarding_permissions(ui),
            Step::Practice => self.onboarding_practice(ui),
            Step::Ready => self.onboarding_ready(ui),
        };
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
                    .flex()
                    .justify_between()
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
                        top.child(ui.button("Skip setup", Action::Onboarding(TourAction::Finish)))
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
                    .pb(EDGE)
                    .flex()
                    .flex_col()
                    .gap(px(24.0))
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
                    .p(EDGE)
                    .pt(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        row()
                            .justify_between()
                            .child(row().when(step != Step::Prepare, |row| {
                                row.child(ui.button("Back", Action::Onboarding(TourAction::Back)))
                            }))
                            .child(if step == Step::Ready {
                                ui.button(
                                    "Start using Speakeasy",
                                    Action::Onboarding(TourAction::Finish),
                                )
                            } else {
                                ui.button(
                                    if step == Step::Permissions {
                                        "Try dictation"
                                    } else {
                                        "Continue"
                                    },
                                    Action::Onboarding(TourAction::Next),
                                )
                                .aria_disabled(!advancing)
                                .when(!advancing, |button| button.opacity(0.45))
                            }),
                    )
                    .child(div().min_h(px(48.0)).child(if is_demo(ui.cx) {
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

    fn onboarding_prepare(&self, ui: &Ui<'_>) -> Div {
        let running = is_demo(ui.cx) || ui.cx.global::<Services>().is_running();
        let needs_setup = !is_demo(ui.cx) && (self.config.needs_engine() || self.is_setting_up());
        div().flex().flex_col().gap(px(20.0)).text_size(px(15.0))
            .child("Your voice becomes text on this computer. No account, no dictation history, no recording uploads.")
            .when(needs_setup, |page| page.child(self.setup_card(ui)))
            .when(!needs_setup, |page| {
                page.child(ui.panel().child(if running {
                    "Local speech is configured. Continue to check access and try your shortcut."
                } else {
                    "Your local speech files are configured. Enable dictation to continue."
                }))
            })
            .when(self.unsaved(ui.cx) && !self.is_setting_up(), |page| {
                page.child(ui.button("Save and enable dictation", Action::Save))
            })
            .when(!needs_setup && !running && !self.unsaved(ui.cx), |page| {
                page.child(ui.button("Enable dictation", Action::Resume))
            })
            .child(div().text_size(TEXT).text_color(rgb(ui.palette.muted))
                .child("The first download needs internet. Once installed, local dictation works offline. Skip returns to Settings, where you can choose your own model. An active download keeps running."))
    }

    fn onboarding_permissions(&self, ui: &Ui<'_>) -> Div {
        let observed = self.permissions.unwrap_or_default();
        let page = div().flex().flex_col().gap(px(20.0)).text_size(px(15.0))
            .child("Speakeasy needs your microphone to hear you and a shortcut to start dictation. Nothing records until you use that shortcut.")
            .child(ui.panel().flex().flex_col().gap(px(12.0))
                .child(format!("Microphone: {}", permissions::description(observed.microphone)))
                .child(if cfg!(target_os = "macos") {
                    "Your first practice recording asks for microphone access. If you previously denied access, enable Speakeasy in System Settings."
                } else if cfg!(target_os = "windows") {
                    "If recording is blocked, enable microphone access for desktop apps in Windows Settings."
                } else {
                    "Practice checks your microphone when you use it. Your desktop may ask for shortcut access; approve it to use the keys shown next."
                })
                .child(row().child(ui.button(
                    if cfg!(target_os = "linux") { "Microphone help" } else { "Open microphone settings" },
                    Action::PermissionHelp(permissions::Help::Microphone),
                ))));
        #[cfg(target_os = "macos")]
        let page = page.child(ui.panel().flex().flex_col().gap(px(12.0))
            .child(format!("Accessibility: {}", permissions::description(observed.accessibility)))
            .child("Enable Speakeasy so it can observe your shortcut and insert text. If macOS requests a restart, quit and reopen Speakeasy; setup remains available.")
            .child(row().child(ui.button("Open Accessibility settings", Action::PermissionHelp(permissions::Help::Accessibility)))));
        page.when(!is_demo(ui.cx) && !ui.cx.global::<Services>().is_running(), |page| {
            page.child(ui.button("Retry dictation after enabling access", Action::Resume))
        }).child(div().text_size(TEXT).text_color(rgb(ui.palette.muted))
            .child("Access status refreshes when you return here. Screen capture is not needed for dictation; it starts only when you ask for a Show."))
    }

    fn onboarding_practice(&self, ui: &Ui<'_>) -> Div {
        let active = self.practice.task.is_some();
        let recording = active && self.presentation.indicator == Indicator::Recording;
        let shortcut = &self.presentation.shortcut;
        let instruction = if cfg!(target_os = "linux") && self.config.linux.external_shortcut {
            format!("Use {shortcut} to start, say a sentence, then use it again to finish.")
        } else {
            format!("Hold {shortcut}, say a sentence, then release.")
        };
        div().flex().flex_col().gap(px(20.0)).text_size(px(15.0))
            .child(instruction)
            .child(ui.panel().flex().flex_col().gap(px(16.0))
                .child(div().text_size(px(24.0)).child(shortcut.to_string()))
                .child(if recording { "Listening…" } else if active && self.presentation.indicator == Indicator::Busy {
                    "Turning speech into text…"
                } else if self.practice.ready {
                    "Ready when you are"
                } else if active { "Preparing private practice…" } else { "Practice is stopped" })
                .child(div().h(px(6.0)).w_full().bg(rgb(ui.palette.raise))
                    .child(div().h_full().w(relative(if recording { self.practice.level } else { 0.0 }))
                        .bg(rgb(ui.palette.live))))
                .child(div().min_h(px(72.0)).child(if self.practice.text.is_empty() {
                    div().text_color(rgb(ui.palette.muted)).child("Your words will appear here.").into_any_element()
                } else {
                    div().child(gpui::text!(id = "onboarding-words", self.practice.text.to_string())).into_any_element()
                }))
                .child(row().child(ui.button(if is_demo(ui.cx) { "Simulate dictation" } else if active { "End practice" } else { "Start practice" }, Action::Practice))))
            .child(if is_demo(ui.cx) { "Demo: microphone and shortcut activity are simulated." } else { "This is real local dictation. Your words stay in this window; nothing is pasted, copied, or saved." })
            .when(!self.practice.text.trim().is_empty(), |page| page.child(
                div().text_color(rgb(ui.palette.lamp)).child("That worked. Continue when you're ready.")))
            .child(self.microphone_section(ui))
            .when(self.unsaved(ui.cx), |page| page.child(ui.button("Save microphone choice", Action::Save)))
            .child(div().text_size(TEXT).text_color(rgb(ui.palette.muted))
                .child("Leaving this window clears practice. If you hear nothing, check your microphone choice and system access, then start practice again."))
    }

    fn onboarding_ready(&self, ui: &Ui<'_>) -> Div {
        let gesture = if cfg!(target_os = "linux") && self.config.linux.external_shortcut {
            "Use your desktop binding to start and finish hands-free dictation.".to_owned()
        } else {
            format!(
                "Hold {} to speak. Double-tap for hands-free, then press again to finish.",
                self.presentation.shortcut
            )
        };
        div().flex().flex_col().gap(px(20.0)).text_size(px(15.0))
            .child(if is_demo(ui.cx) { "Demo complete. Real setup uses your microphone and local speech model; this preview used neither." } else { "You just turned speech into text locally. Your practice words have been cleared." })
            .child(ui.panel().flex().flex_col().gap(px(12.0))
                .child(gesture)
                .child(format!("{} cancels a recording without delivering it.", self.presentation.cancel_shortcut)))
            .child("Next, open an editor, place the cursor, and dictate a sentence. That checks delivery into another app; private practice does not test pasting or typing there.")
            .child(div().text_size(TEXT).text_color(rgb(ui.palette.muted))
                .child("Replay this guide any time from Settings → Advanced → Replay setup."))
    }
}
