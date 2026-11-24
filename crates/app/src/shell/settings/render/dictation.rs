use std::path::Path;

use gpui::{AnyElement, Div, FontWeight, SharedString, div, prelude::*, px, rgb};
#[cfg(not(target_os = "linux"))]
use speakeasy_platform::Binding;

use super::{Ui, fraction, progress_text, setup_progress};
#[cfg(not(target_os = "linux"))]
use crate::controls::{CORNER, TITLE_TEXT};
#[cfg(not(target_os = "linux"))]
use crate::shell::settings::shortcuts::Edit;
use crate::{
    config::Engine,
    controls::{PANEL_CORNER, Tone, Weight},
    models::{self, Model},
    shell::{
        Services,
        settings::{
            Action, EngineFile, Setting, Settings, is_demo,
            menu::{self, Menu},
            speech::{self, Speech},
            vocabulary,
        },
    },
    status::Indicator,
};

/// macOS Secure Keyboard Entry hides every key but modifiers from the shortcut tap.
const SECURE_KEYS: &str = "While Secure Keyboard Entry is on, as in a password field, macOS hides these keys from Speakeasy.";

/// The keys held while a shortcut records, announced as they change.
#[cfg(not(target_os = "linux"))]
fn recording_keys(
    ui: &Ui<'_>,
    holding: Option<speakeasy_platform::NativeShortcut>,
    large: bool,
) -> impl IntoElement {
    let palette = ui.palette;
    let label = holding.map(speakeasy_platform::NativeShortcut::label);
    let keys = match &label {
        Some(label) => ui.kit().keys(label, large),
        None => div()
            .h(px(if large { 40.0 } else { 26.0 }))
            .px(px(12.0))
            .flex()
            .items_center()
            .rounded(CORNER)
            .border_1()
            .border_color(palette.edge())
            .text_size(px(if large { 14.0 } else { 12.0 }))
            .text_color(rgb(palette.muted))
            .whitespace_nowrap()
            .child("Press keys…"),
    };
    div()
        .id("shortcut-recording")
        .role(gpui::Role::Status)
        .aria_label(label.unwrap_or_else(|| "Recording a shortcut".into()))
        .child(keys)
}

impl Settings {
    pub(super) fn dictation_page(&self, ui: &Ui<'_>) -> Vec<AnyElement> {
        [
            Some(self.shortcut_card(ui).into_any_element()),
            #[cfg(not(target_os = "linux"))]
            Some(self.more_shortcuts(ui).into_any_element()),
            self.session_preset(ui).map(IntoElement::into_any_element),
            Some(self.speech_section(ui).into_any_element()),
            Some(self.writing_section(ui).into_any_element()),
            Some(self.presets_section(ui).into_any_element()),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub(super) fn shortcut(&self) -> SharedString {
        cfg_select! {
            target_os = "linux" => SharedString::from(&self.presentation.shortcut),
            _ => self.config.shortcuts().talk.label().into(),
        }
    }

    /// The hold-to-talk keys, or the keys held while they record.
    pub(super) fn talk_keys(&self, ui: &Ui<'_>, large: bool) -> AnyElement {
        #[cfg(not(target_os = "linux"))]
        if self.shortcuts.is_recording(Binding::Talk) {
            return recording_keys(ui, self.shortcuts.holding(), large).into_any_element();
        }
        ui.kit().keys(&self.shortcut(), large).into_any_element()
    }

    pub(super) fn cancel_shortcut(&self) -> String {
        self.presentation.cancel_shortcut.replace("Escape", "Esc")
    }

    pub(super) fn uses_desktop_bindings(&self) -> bool {
        cfg!(target_os = "linux") && self.config.linux.external_shortcut
    }

    fn shortcut_card(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let bindings = self.uses_desktop_bindings();
        let (title, detail) = if bindings {
            (
                "Your desktop shortcut",
                "Press it to start dictating, and again to finish.",
            )
        } else {
            (
                "Hold to talk",
                "Release, and your words appear where you're typing.",
            )
        };
        let heading = div()
            .flex()
            .items_center()
            .gap(px(18.0))
            .px(px(16.0))
            .pt(px(20.0))
            .pb(px(18.0))
            .when(!bindings, |heading| heading.child(self.talk_keys(ui, true)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(16.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child({
                        #[cfg(not(target_os = "linux"))]
                        let detail = self.binding_detail(ui, Binding::Talk, detail);
                        #[cfg(target_os = "linux")]
                        let detail = kit.detail(detail);
                        detail
                    }),
            );
        #[cfg(not(target_os = "linux"))]
        let heading = heading.child(self.shortcut_choice(ui));
        let legend = div()
            .flex()
            .flex_wrap()
            .gap_x(px(28.0))
            .gap_y(px(8.0))
            .px(px(16.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(palette.line())
            .text_size(px(13.0))
            .when(!bindings, |legend| {
                legend.child(legend_entry(
                    ui,
                    "Hands-free",
                    if cfg!(target_os = "linux") {
                        div().child("Double-tap the shortcut")
                    } else {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child("Double-tap, or hold and add")
                            .child(kit.keycap("Space", false))
                    },
                ))
            })
            .child(legend_entry(
                ui,
                "Cancel",
                if bindings {
                    div().child("Your desktop's cancel binding")
                } else {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(kit.keys(&self.cancel_shortcut(), false))
                        .child("while talking")
                },
            ));
        // Linux shortcuts arrive as portal activations, so no key reaches Speakeasy during a hold.
        #[cfg(not(target_os = "linux"))]
        let legend = {
            let held = |name: &'static str, key: &'static str| {
                legend_entry(
                    ui,
                    name,
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(kit.keycap(key, false))
                        .child("while holding"),
                )
            };
            let send = if cfg!(target_os = "macos") {
                "Return"
            } else {
                "Enter"
            };
            legend
                .child(held("Send", send))
                .child(held("Paste again", "V"))
        };
        div()
            .flex()
            .flex_col()
            .rounded(PANEL_CORNER)
            .border_1()
            .border_color(palette.line())
            .bg(rgb(palette.panel))
            .child(heading)
            .child(legend)
            .children(
                self.shortcut_help()
                    .map(|help| kit.detail(help).px(px(16.0)).pb(px(12.0))),
            )
            .children(
                self.globe_warning(ui)
                    .map(|warning| warning.mx(px(16.0)).mb(px(16.0))),
            )
            .child(self.practice_panel(ui))
    }

    /// The Fn shortcut's other meaning on macOS, which fires with every Speakeasy press too.
    fn globe_warning(&self, ui: &Ui<'_>) -> Option<Div> {
        use speakeasy_platform::{GlobeKey, MacShortcut};
        if self.config.mac_shortcut != MacShortcut::FUNCTION {
            return None;
        }
        let (title, effect) = match self.globe_key? {
            GlobeKey::Emoji => (
                "Fn also opens Emoji & Symbols",
                "The picker opens on every Fn press, including while you dictate, and can catch the text Speakeasy pastes.",
            ),
            GlobeKey::InputSource => (
                "Fn also switches the input source",
                "macOS switches the input source on every Fn press, including while you dictate.",
            ),
            GlobeKey::Dictation => (
                "Double-tapping Fn also starts macOS Dictation",
                "macOS starts its own Dictation when you double-tap Fn, which is how Speakeasy goes hands-free.",
            ),
        };
        Some(ui.kit().banner(
            Tone::Warn,
            title,
            format!(
                "{effect} In Keyboard settings, set “Press 🌐 key to” to Do Nothing, or choose another shortcut."
            ),
            Some(div().child(ui.button(
                "Open Keyboard Settings",
                Action::KeyboardSettings,
                Weight::Secondary,
            ))),
        ))
    }

    /// A caveat of the hold-to-talk shortcut, where it has one; Linux binds its own.
    pub(super) fn shortcut_help(&self) -> Option<&'static str> {
        use speakeasy_platform::{MacShortcut, WindowsShortcut};
        const QUICK: &str = "Hold briefly before adding Space, Return, S, W, A, or V. Quick combinations keep their usual action.";
        if cfg!(target_os = "linux") {
            return None;
        }
        let preset = if cfg!(target_os = "macos") {
            match self.config.mac_shortcut {
                MacShortcut::FUNCTION => Some(
                    "External keyboard? Its Fn key may stay inside the keyboard. Choose another shortcut if Fn does nothing.",
                ),
                MacShortcut::F13 => Some(
                    "Use your keyboard's F13 key, or map a key to F13 in its keyboard software. Speakeasy reserves F13 while dictation is enabled.",
                ),
                _ => None,
            }
        } else {
            match self.config.windows_shortcut {
                WindowsShortcut::CONTROL_WINDOWS => Some(
                    "Hold Ctrl and Win together. Choose another shortcut if an app already uses Ctrl+Win.",
                ),
                WindowsShortcut::F13 => Some(
                    "Map a spare key, such as Caps Lock, to F13 in PowerToys Keyboard Manager or your keyboard's software. Speakeasy reserves F13 while dictation is enabled.",
                ),
                _ => None,
            }
        };
        let talk = self.config.shortcuts().talk;
        preset
            .or_else(|| talk.is_lone_modifier().then_some(QUICK))
            .or_else(|| (cfg!(target_os = "macos") && talk.has_key()).then_some(SECURE_KEYS))
    }

    /// A shortcut row's line under its title: its meaning, the recording under way, or why the
    /// keys just recorded were refused.
    #[cfg(not(target_os = "linux"))]
    fn binding_detail(&self, ui: &Ui<'_>, binding: Binding, meaning: &'static str) -> Div {
        let kit = ui.kit();
        if self.shortcuts.is_recording(binding) {
            return kit.detail("Press the keys, or Esc to cancel.");
        }
        if let Some((keys, refusal)) = self.shortcuts.refused(binding) {
            let why = crate::config::refusal_message(refusal, self.config.shortcuts().talk);
            return kit
                .detail(format!("{}: {why}", keys.label()))
                .text_color(rgb(ui.palette.warn));
        }
        kit.detail(meaning)
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn shortcut_choice(&self, ui: &Ui<'_>) -> Div {
        if self.shortcuts.is_recording(Binding::Talk) {
            return div().child(ui.button("Cancel", Action::Shortcut(Edit::Cancel), Weight::Quiet));
        }
        let menu = Menu::Shortcut;
        let action = Action::Menu(menu);
        let trigger = ui
            .button("Change", action, Weight::Secondary)
            .aria_label(format!("Shortcut key: {}", self.shortcut()))
            .child(crate::controls::icon(
                crate::icons::CHEVRON_DOWN,
                14.0,
                ui.palette.muted,
            ));
        self.anchor_menu(ui, menu, "Shortcut key", trigger, true)
    }

    fn practice_panel(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let panel = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(16.0))
            .py(px(16.0))
            .border_t_1()
            .border_color(palette.line());
        let Some(live) = &self.practice.live else {
            let running = ui.cx.global::<Services>().is_running() && !is_demo(ui.cx);
            let note = if is_demo(ui.cx) {
                "Practice uses your microphone, so the preview leaves it off."
            } else if self.config.needs_engine() {
                "Available once local speech is set up."
            } else if !running {
                "Resume dictation to practice."
            } else {
                "Practice stays in this window and clears when you leave. Nothing is pasted or saved."
            };
            return panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(kit.label("Try it here", note).flex_1())
                    .child(ui.button_if(
                        "Start practice",
                        Action::Practice,
                        Weight::Secondary,
                        running,
                    )),
            );
        };
        let indicator = self.presentation.indicator;
        let result = if !self.practice.text.is_empty() {
            div().flex().flex_wrap().gap(px(3.0)).children(
                vocabulary::practice_words(&self.practice.text)
                    .enumerate()
                    .map(|(index, word)| {
                        let action = Action::Correction(vocabulary::Edit::FromPractice(index));
                        ui.kit().word(
                            action.element_id(),
                            word.to_owned(),
                            self.picked_word(index),
                            ui.press(action),
                        )
                    }),
            )
        } else if indicator == Indicator::Recording {
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(level(palette.live, live.level))
                .child("Listening…")
        } else if indicator == Indicator::Busy {
            div()
                .text_color(rgb(palette.muted))
                .child("Transcribing locally…")
        } else if live.ready {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .child("Hold")
                .child(kit.keys(&self.shortcut(), false))
                .child("and say a sentence.")
        } else {
            div()
                .text_color(rgb(palette.muted))
                .child("Preparing practice…")
        };
        panel
            .child(
                div()
                    .id("practice-result")
                    .role(gpui::Role::Status)
                    .min_h(px(40.0))
                    .max_h(px(140.0))
                    .overflow_y_scroll()
                    .flex()
                    .items_center()
                    .px(px(14.0))
                    .py(px(10.0))
                    .rounded(px(8.0))
                    .bg(rgb(palette.room))
                    .border_1()
                    .border_color(palette.line())
                    .child(result),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        kit.detail(
                            "Choose a word Speakeasy got wrong to correct it; choose its neighbor to make a phrase.",
                        )
                        .flex_1(),
                    )
                    .child(ui.button("End practice", Action::Practice, Weight::Quiet)),
            )
            .children(self.practice_correction(ui))
    }

    /// The optional shortcuts that each start an action and, pressed again, finish it, then the
    /// Show gestures. Folded away so the page opens on hold to talk and the speech settings.
    #[cfg(not(target_os = "linux"))]
    fn more_shortcuts(&self, ui: &Ui<'_>) -> Div {
        use speakeasy_platform::{Scope, Toggle};
        let rows = [
            (
                Toggle::HandsFree,
                "Press to start dictating, and again to finish.",
            ),
            (
                Toggle::Show(Scope::Screen),
                "Press to record the screen, and again to finish.",
            ),
            (
                Toggle::Show(Scope::Window),
                "Press to record a window, and again to finish.",
            ),
            (
                Toggle::Show(Scope::Area),
                "Press to record an area, and again to finish.",
            ),
        ]
        .map(|(toggle, meaning)| self.toggle_row(ui, toggle, meaning).into_any_element());
        let unavailable = if self.can_record_shortcut() {
            None
        } else if is_demo(ui.cx) {
            Some("The preview can't record shortcuts.")
        } else if self.presentation.in_session {
            Some("Finish dictating to record a shortcut.")
        } else {
            Some("Resume dictation to record a shortcut.")
        };
        let kit = ui.kit();
        let disclosure = kit.disclosure(
            Action::MoreShortcuts.element_id(),
            "Hands-free and Show shortcuts",
            self.more_shortcuts,
            ui.press(Action::MoreShortcuts),
        );
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().flex().child(disclosure))
            .when(self.more_shortcuts, |section| {
                section
                    .child(kit.group(rows))
                    .children(unavailable.map(|note| ui.text(note)))
                    .when(cfg!(target_os = "macos"), |section| {
                        section.child(ui.text(SECURE_KEYS))
                    })
                    .child(self.show_gestures(ui))
            })
    }

    #[cfg(not(target_os = "linux"))]
    fn toggle_row(
        &self,
        ui: &Ui<'_>,
        toggle: speakeasy_platform::Toggle,
        meaning: &'static str,
    ) -> Div {
        let kit = ui.kit();
        let binding = Binding::Toggle(toggle);
        let record = Action::Shortcut(Edit::Record(binding));
        let can_record = self.can_record_shortcut();
        let controls = div().flex_none().flex().items_center().gap(px(8.0));
        let controls = match (
            self.shortcuts.is_recording(binding),
            self.config.shortcuts().toggles.get(toggle),
        ) {
            (true, _) => controls
                .child(recording_keys(ui, self.shortcuts.holding(), false))
                .child(ui.button("Cancel", Action::Shortcut(Edit::Cancel), Weight::Quiet)),
            (false, Some(keys)) => controls
                .child(kit.keys(&keys.label(), false))
                .child(ui.button_if("Change", record, Weight::Secondary, can_record))
                .child(ui.button(
                    "Remove",
                    Action::Shortcut(Edit::Remove(toggle)),
                    Weight::Quiet,
                )),
            (false, None) => {
                controls.child(ui.button_if("Record", record, Weight::Secondary, can_record))
            },
        };
        div()
            .flex()
            .items_center()
            .gap(px(16.0))
            .min_h(px(60.0))
            .px(px(16.0))
            .py(px(12.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(TITLE_TEXT)
                            .text_color(rgb(ui.palette.ink))
                            .child(crate::config::binding_title(binding)),
                    )
                    .child(self.binding_detail(ui, binding, meaning)),
            )
            .child(controls)
    }

    /// What the hold-to-talk shortcut does with Shows, for reference.
    #[cfg(not(target_os = "linux"))]
    fn show_gestures(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let shortcut = self.shortcut();
        let word = |text: &'static str| div().text_color(rgb(palette.muted)).child(text);
        let key = |text: &'static str| kit.keycap(text, false);
        let keys = || kit.keys(&shortcut, false);
        let line = |chord: Vec<Div>, meaning: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(16.0))
                .px(px(16.0))
                .py(px(10.0))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .text_size(px(12.0))
                        .children(chord),
                )
                .child(kit.detail(meaning).flex_1().min_w(px(0.0)).text_right())
                .into_any_element()
        };
        kit.group([
            line(
                vec![keys(), word("+"), key("S"), key("W"), key("A")],
                "Record the screen, a window, or an area",
            ),
            line(
                vec![word("Hold"), key("Shift"), word("and move")],
                "Draw, without clicking",
            ),
            line(
                vec![key("Shift"), key("Shift"), word("twice")],
                "Pin what you drew",
            ),
            line(
                vec![word("Press"), keys(), word("again")],
                "Finish the Show",
            ),
            line(vec![keys(), word("+"), key("Esc")], "Cancel the Show"),
        ])
    }

    fn speech_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let mut rows = vec![self.model_row(ui).into_any_element()];
        if self.config.speech_configured() && !speech::detects_language(&self.config) {
            rows.push(
                kit.setting(
                    "Language",
                    if self.config.uses_nemotron() {
                        "Nemotron transcribes the language you choose."
                    } else {
                        "Whisper can detect it, or you can choose."
                    },
                    self.dropdown(
                        ui,
                        Menu::Language,
                        "Language",
                        menu::language_name(&self.config),
                    ),
                )
                .into_any_element(),
            );
        }
        rows.push(
            kit.setting(
                "Microphone",
                if self.config.microphone.is_none() {
                    "Follows your system's input device."
                } else {
                    "Speakeasy uses this microphone whenever it's connected, and the system default otherwise."
                },
                self.dropdown(ui, Menu::Microphone, "Microphone", self.microphone_name()),
            )
            .into_any_element(),
        );
        kit.section("Speech", rows)
    }

    pub(super) fn microphone_name(&self) -> SharedString {
        match &self.config.microphone {
            None => "System default".into(),
            Some(id) => match self
                .microphones
                .iter()
                .find(|row| row.present && row.device.id == *id)
            {
                Some(row) => row.device.name.as_str().into(),
                None if self.microphone_listing.is_some() => "Checking…".into(),
                None => "Unavailable microphone".into(),
            },
        }
    }

    fn model_summary(&self, ui: &Ui<'_>) -> (SharedString, bool) {
        if is_demo(ui.cx) {
            return (crate::shell::services::SIMULATED_MODEL.into(), false);
        }
        match Speech::of(&self.config, self.own_engine) {
            Some(Speech::Managed(Model::Parakeet)) => (
                "Parakeet v3 · 25 European languages, detected as you speak".into(),
                false,
            ),
            Some(Speech::Managed(Model::Nemotron)) => (
                "Nemotron 3.5 · 32 locales, in the language you choose".into(),
                false,
            ),
            Some(Speech::Own) if self.config.speech_configured() => (
                format!("Your own engine · {}", file_name(&self.config.model)).into(),
                false,
            ),
            Some(Speech::Own) => ("Your own engine · Choose its files".into(), true),
            None if self.engine_work.installing().is_some() => ("Setting up…".into(), false),
            None => ("Not set up".into(), true),
        }
    }

    fn model_row(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let (summary, warn) = self.model_summary(ui);
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.0))
                    .min_h(px(60.0))
                    .px(px(16.0))
                    .py(px(12.0))
                    .child(
                        kit.label("Speech model", summary)
                            .flex_1()
                            .when(warn, |label| label.text_color(rgb(ui.palette.warn))),
                    )
                    .child(ui.button_if(
                        if self.choosing_model {
                            "Done"
                        } else {
                            "Change"
                        },
                        Action::Models,
                        Weight::Secondary,
                        !is_demo(ui.cx),
                    )),
            )
            .when(self.choosing_model, |row| row.child(self.model_chooser(ui)))
    }

    fn model_chooser(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let speech = Speech::of(&self.config, self.own_engine);
        let installing = self
            .engine_work
            .installing()
            .map(|installing| (installing.model, installing.setup.progress()));
        let busy = self.engine_work.is_busy();
        let card = |action: Action,
                    title: &'static str,
                    about: &'static str,
                    tag: Option<(SharedString, Tone)>,
                    chosen: bool| {
            kit.radio_card(
                action.element_id(),
                title,
                chosen,
                (!busy && !chosen).then(|| ui.press(action)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().text_size(px(14.0)).child(title))
                            .when_some(tag, |title, (tag, tone)| title.child(kit.tag(tag, tone))),
                    )
                    .child(kit.detail(about)),
            )
        };
        let managed = |model: Model, about: &'static str, size: &'static str| {
            let chosen = speech == Some(Speech::Managed(model));
            let tag = match installing {
                Some((loading, progress)) if loading == model => (
                    format!("{}%", (fraction(progress) * 100.0).round()).into(),
                    Tone::Lamp,
                ),
                _ if chosen => ("In use".into(), Tone::Lamp),
                _ if self.downloaded.contains(&model) => ("Downloaded".into(), Tone::Muted),
                _ => (size.into(), Tone::Muted),
            };
            card(
                Action::Install(model),
                model.label(),
                about,
                Some(tag),
                chosen,
            )
        };
        let own = speech == Some(Speech::Own);
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .px(px(16.0))
            .pb(px(16.0))
            .child(
                div()
                    .id("speech-models")
                    .role(gpui::Role::RadioGroup)
                    .aria_label("Speech model")
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(managed(
                        Model::Parakeet,
                        "Recommended. Fast and accurate in 25 European languages, detected as you speak.",
                        "714 MB",
                    ))
                    .child(managed(
                        Model::Nemotron,
                        "32 locales, including Hindi, Japanese, Korean, and Mandarin. Follows the language you choose and your recognition hints.",
                        "742 MB",
                    ))
                    .child(card(
                        Action::OwnEngine,
                        "Your own engine",
                        "A whisper.cpp or NeMo-Speech build you installed, with a model file you choose.",
                        own.then(|| ("In use".into(), Tone::Lamp)),
                        own,
                    )),
            )
            .when_some(installing, |chooser, (model, progress)| {
                chooser.child(setup_progress(
                    ui,
                    progress,
                    format!("{} · {}", model.label(), progress_text(progress)),
                ))
            })
            .when(own, |chooser| chooser.child(self.own_engine_panel(ui)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.0))
                    .pt(px(4.0))
                    .child(
                        kit.label(
                            "Prefer GPU",
                            "Use your graphics card for speech when it can.",
                        )
                        .flex_1(),
                    )
                    .child(ui.switch(
                        "Prefer GPU",
                        self.config.use_gpu,
                        Action::Toggle(Setting::PreferGpu),
                    )),
            )
    }

    fn own_engine_panel(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let engine = self.config.engine;
        let (program, model, hint) = match engine {
            Engine::Whisper => (
                "whisper-server program",
                "Whisper model (.bin)",
                "Use whisper-server from a whisper.cpp release and a GGML model.",
            ),
            Engine::Nemo => (
                "nemo-speech program",
                "NeMo model (.gguf)",
                "Use nemo-speech from NeMo-Speech.cpp and a GGUF speech model.",
            ),
        };
        let can_choose = !self.engine_work.is_busy();
        let row = |kind: &'static str, path: &Path, action: Action| {
            div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(kit.label(kind, file_name(path)).flex_1())
                .child(ui.button_if("Choose…", action, Weight::Secondary, can_choose))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(12.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(ui.palette.line())
            .bg(rgb(ui.palette.room))
            .child(
                div().flex().child(
                    kit.segmented("engines", "Engine").children(
                        [
                            (Engine::Whisper, "whisper.cpp"),
                            (Engine::Nemo, "NeMo-Speech"),
                        ]
                        .map(|(choice, label)| {
                            let action = Action::Engine(choice);
                            kit.segment(
                                action.element_id(),
                                label,
                                engine == choice,
                                Some(ui.press(action)),
                            )
                        }),
                    ),
                ),
            )
            .child(row(
                program,
                &self.config.engine_executable,
                Action::Choose(EngineFile::Executable),
            ))
            .child(row(
                model,
                &self.config.model,
                Action::Choose(EngineFile::Model),
            ))
            .child(kit.detail(if self.config.speech_configured() {
                hint
            } else {
                "Choose both files to switch to your own engine."
            }))
    }

    fn writing_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let detects = speech::detects_language(&self.config);
        let english = models::is_english(menu::language(&self.config));
        let (enabled, detail) = if detects {
            (
                true,
                "Removes standalone um and uh from English. Leave off if you also dictate in a language where “um” is a word.",
            )
        } else if english {
            (true, "Removes standalone um and uh.")
        } else if self.config.language == "auto" {
            (false, "Choose English as the language to remove um and uh.")
        } else {
            (false, "For English only. Other languages keep every word.")
        };
        let fillers = kit.setting(
            "Remove um and uh",
            detail,
            ui.switch_if(
                "Remove um and uh",
                Setting::RemoveFillers.is_on(&self.config),
                Action::Toggle(Setting::RemoveFillers),
                enabled,
            ),
        );
        let symbols = kit.setting(
            "Spoken symbols",
            "Saying “new line”, “new paragraph”, “underscore”, “open paren”, and similar phrases writes their symbol. The Verbatim dictation style in Speakeasy's menu keeps the words.",
            ui.switch(
                "Spoken symbols",
                Setting::SpokenSymbols.is_on(&self.config),
                Action::Toggle(Setting::SpokenSymbols),
            ),
        );
        let scripts = kit.setting(
            "Let scripts receive dictations",
            "A script that runs speakeasy dictation start --print receives the words instead of having them pasted. The pill shows “To script” while it records.",
            ui.switch(
                "Let scripts receive dictations",
                Setting::PrintToScripts.is_on(&self.config),
                Action::Toggle(Setting::PrintToScripts),
            ),
        );
        #[cfg(not(target_os = "linux"))]
        let delivery = {
            use crate::config::TextDelivery;
            let typed = self.config.text_delivery == TextDelivery::Type;
            kit.setting(
                "Type instead of paste",
                "Pasting puts your previous clipboard back a moment later. Typing never touches the clipboard, but some editors reject it.",
                kit.switch(
                    "type-instead-of-paste",
                    "Type instead of paste",
                    typed,
                    Some(ui.press(Action::SelectDelivery(if typed {
                        TextDelivery::Paste
                    } else {
                        TextDelivery::Type
                    }))),
                ),
            )
        };
        #[cfg(target_os = "linux")]
        let delivery = kit.setting(
            "Insert text by",
            "How your words reach the app you're using.",
            self.dropdown(
                ui,
                Menu::Delivery,
                "Insert text by",
                menu::delivery_label(self.config.text_delivery),
            ),
        );
        kit.section(
            "Writing",
            vec![
                fillers.into_any_element(),
                symbols.into_any_element(),
                delivery.into_any_element(),
                scripts.into_any_element(),
            ],
        )
    }
}

fn legend_entry(ui: &Ui<'_>, name: &'static str, value: Div) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(ui.palette.muted))
                .child(name),
        )
        .child(value)
}

pub(super) fn level(color: u32, level: f32) -> Div {
    div()
        .flex()
        .items_end()
        .gap(px(3.0))
        .h(px(16.0))
        .children([0.55, 0.85, 1.0, 0.75, 0.5].map(|weight: f32| {
            div()
                .w(px(3.0))
                .h(px(3.0 + 13.0 * (level * weight).clamp(0.0, 1.0)))
                .rounded(px(1.5))
                .bg(rgb(color))
        }))
}

fn file_name(path: &Path) -> SharedString {
    path.file_name().map_or_else(
        || SharedString::from("Not chosen"),
        |name| name.to_string_lossy().into(),
    )
}
