use std::path::Path;

use gpui::{AnyElement, Div, FontWeight, SharedString, div, prelude::*, px, rgb};

use super::{Ui, fraction, progress_text};
use crate::{
    config::Engine,
    controls::{Tone, Weight},
    models::Model,
    shell::{
        Services,
        settings::{
            Action, EngineFile, Setting, Settings, is_demo,
            menu::{self, Menu},
            speech::{self, Speech},
        },
    },
    status::Indicator,
};

impl Settings {
    pub(super) fn dictation_page(&self, ui: &Ui<'_>) -> Vec<AnyElement> {
        [
            Some(self.shortcut_card(ui).into_any_element()),
            (!cfg!(target_os = "linux")).then(|| self.show_shortcuts(ui).into_any_element()),
            Some(self.speech_section(ui).into_any_element()),
            Some(self.writing_section(ui).into_any_element()),
            Some(self.presets_section(ui).into_any_element()),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub(super) fn shortcut(&self) -> SharedString {
        #[cfg(target_os = "macos")]
        return self.config.mac_shortcut.label().into();
        #[cfg(not(target_os = "macos"))]
        self.presentation.shortcut.to_string().into()
    }

    fn shortcut_card(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let bindings = cfg!(target_os = "linux") && self.config.linux.external_shortcut;
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
            .px(px(20.0))
            .pt(px(20.0))
            .pb(px(18.0))
            .when(!bindings, |heading| {
                heading.child(kit.keys(&self.shortcut(), true))
            })
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
                    .child(kit.detail(detail)),
            );
        #[cfg(target_os = "macos")]
        let heading = heading.child(self.mac_shortcut(ui));
        let cancel = self.presentation.cancel_shortcut.replace("Escape", "Esc");
        let legend = div()
            .flex()
            .flex_wrap()
            .gap_x(px(28.0))
            .gap_y(px(8.0))
            .px(px(20.0))
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
                        .child(kit.keys(&cancel, false))
                        .child(div().text_color(rgb(palette.muted)).child("while talking"))
                },
            ));
        div()
            .flex()
            .flex_col()
            .rounded(px(12.0))
            .border_1()
            .border_color(palette.line())
            .bg(rgb(palette.panel))
            .child(heading)
            .child(legend)
            .when(
                cfg!(target_os = "macos")
                    && self.config.mac_shortcut != speakeasy_platform::MacShortcut::Function,
                |card| {
                    card.child(kit.detail("Hold briefly before adding Space, S, W, or A. Quick combinations keep their usual action.").px(px(20.0)).pb(px(12.0)))
                },
            )
            .child(self.practice_panel(ui))
    }

    #[cfg(target_os = "macos")]
    fn mac_shortcut(&self, ui: &Ui<'_>) -> Div {
        let menu = Menu::Shortcut;
        let action = Action::Menu(menu);
        let trigger = ui
            .button("Change", action, Weight::Secondary)
            .aria_label(format!(
                "Shortcut key: {}",
                self.config.mac_shortcut.label()
            ))
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
            .px(px(20.0))
            .py(px(16.0))
            .border_t_1()
            .border_color(palette.line());
        if self.practice.task.is_none() {
            let running = ui.cx.global::<Services>().is_running() && !is_demo(ui.cx);
            let note = if is_demo(ui.cx) {
                "Practice uses your microphone, so the preview leaves it off."
            } else if self.config.needs_engine() {
                "Available once local speech is set up."
            } else if !running {
                "Resume dictation to practice."
            } else {
                "Your words stay in this window. Nothing is pasted or saved."
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
        }
        let indicator = self.presentation.indicator;
        let live = if !self.practice.text.is_empty() {
            div().text_size(px(16.0)).child(gpui::text!(
                id = "practice-words",
                self.practice.text.to_string()
            ))
        } else if indicator == Indicator::Recording {
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(level(palette.live, self.practice.level))
                .child("Listening…")
        } else if indicator == Indicator::Busy {
            div()
                .text_color(rgb(palette.muted))
                .child("Turning speech into text…")
        } else if self.practice.ready {
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
                    .child(live),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        kit.detail(
                            "Nothing is pasted or saved. Practice clears when you leave this window.",
                        )
                        .flex_1(),
                    )
                    .child(ui.button("End practice", Action::Practice, Weight::Quiet)),
            )
    }

    fn show_shortcuts(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let shortcut = self.shortcut();
        let toggle = kit.disclosure(
            Action::ShowsHelp.element_id(),
            "Shortcuts for Shows",
            self.shows_help,
            ui.press(Action::ShowsHelp),
        );
        let word = |text: &'static str| div().text_color(rgb(palette.muted)).child(text);
        let key = |text: &'static str| kit.keycap(text, false);
        let keys = || kit.keys(&shortcut, false);
        let line = |chord: Vec<AnyElement>, meaning: &'static str| {
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
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().flex().child(toggle))
            .when(self.shows_help, |section| {
                section.child(kit.group(vec![
                    line(
                        vec![
                            keys().into_any_element(),
                            word("+").into_any_element(),
                            key("S").into_any_element(),
                            key("W").into_any_element(),
                            key("A").into_any_element(),
                        ],
                        "Record the screen, a window, or an area",
                    ),
                    line(
                        vec![
                            word("Hold").into_any_element(),
                            key("Shift").into_any_element(),
                            word("and move").into_any_element(),
                        ],
                        "Draw, without clicking",
                    ),
                    line(
                        vec![
                            key("Shift").into_any_element(),
                            key("Shift").into_any_element(),
                            word("twice").into_any_element(),
                        ],
                        "Pin what you drew",
                    ),
                    line(
                        vec![
                            word("Press").into_any_element(),
                            keys().into_any_element(),
                            word("again").into_any_element(),
                        ],
                        "Finish the Show",
                    ),
                    line(
                        vec![
                            keys().into_any_element(),
                            word("+").into_any_element(),
                            key("Esc").into_any_element(),
                        ],
                        "Cancel the Show",
                    ),
                ]))
            })
    }

    fn speech_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let mut rows = vec![self.model_row(ui).into_any_element()];
        if self.config_complete() && !speech::detects_language(&self.config) {
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
                    "Speakeasy always uses this one."
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
            Some(id) => match self.microphones.iter().find(|device| device.id == *id) {
                Some(device) => device.name.clone().into(),
                None if self.microphone_listing.is_some() => "Checking…".into(),
                None => "Unavailable microphone".into(),
            },
        }
    }

    fn model_summary(&self, ui: &Ui<'_>) -> (SharedString, bool) {
        if is_demo(ui.cx) {
            return ("Simulated model".into(), false);
        }
        match Speech::of(&self.config, self.own_engine) {
            Some(Speech::Managed(Model::Parakeet)) => (
                "Parakeet v3 · 25 European languages, detected as you speak".into(),
                false,
            ),
            Some(Speech::Managed(Model::Nemotron)) => (
                "Nemotron 3.5 · Follows your language and recognition hints".into(),
                false,
            ),
            Some(Speech::Own) if self.config_complete() => (
                format!("Your own engine · {}", file_name(&self.config.model)).into(),
                false,
            ),
            Some(Speech::Own) => ("Your own engine · Choose its files".into(), true),
            None if self.setup.is_some() => ("Setting up…".into(), false),
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
            .setup
            .as_ref()
            .map(|installing| (installing.model, installing.setup.progress()));
        let busy = self.setup.is_some() || self.retiring_setup.is_some();
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
                chooser.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(kit.progress(fraction(progress)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .child(
                                    kit.detail(format!(
                                        "{} · {}",
                                        model.label(),
                                        progress_text(progress)
                                    ))
                                    .flex_1(),
                                )
                                .child(ui.button("Cancel", Action::Setup, Weight::Quiet)),
                        ),
                )
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
        let picking = self.file_picker.is_none();
        let row = |kind: &'static str, path: &Path, action: Action| {
            div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(kit.label(kind, file_name(path)).flex_1())
                .child(ui.button_if("Choose…", action, Weight::Secondary, picking))
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
            .child(kit.detail(if self.config_complete() {
                hint
            } else {
                "Choose both files to switch to your own engine."
            }))
    }

    fn writing_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let detects = speech::detects_language(&self.config);
        let english = menu::language(&self.config)
            .split('-')
            .next()
            .is_some_and(|code| code.eq_ignore_ascii_case("en"));
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
        #[cfg(not(target_os = "linux"))]
        let delivery = {
            use crate::config::TextDelivery;
            let keep = self.config.text_delivery == TextDelivery::Type;
            kit.setting(
                "Keep clipboard",
                "Type text instead of pasting it, so your clipboard stays as it was.",
                ui.switch(
                    "Keep clipboard",
                    keep,
                    Action::SelectDelivery(if keep {
                        TextDelivery::Paste
                    } else {
                        TextDelivery::Type
                    }),
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
            vec![fillers.into_any_element(), delivery.into_any_element()],
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
        |name| name.to_string_lossy().into_owned().into(),
    )
}
