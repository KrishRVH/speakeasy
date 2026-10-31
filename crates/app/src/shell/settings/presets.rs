//! Explicit named snapshots share Settings' draft, pending editors, and durable save queue.

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, SharedString, Window, div, prelude::*, px,
    rgb,
};

use super::{
    Notice, Services, Settings,
    text_field::{Submit, TextField},
};
use crate::{
    config::presets::{self, Preset},
    theme::{Theme, alpha},
};

pub(super) struct Editor {
    name: Entity<TextField>,
}

impl Editor {
    pub(super) fn new(theme: Theme, cx: &mut Context<'_, Settings>) -> Self {
        let name = cx.new(|cx| TextField::new("New preset name", theme, cx));
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&name, |settings, _, _: &Submit, cx| {
            if !settings.presets_busy() {
                settings.capture_preset(None, cx);
            }
        })
        .detach();
        Self { name }
    }

    pub(super) fn pending(&self, cx: &App) -> bool {
        !self.name.read(cx).text().is_empty()
    }

    pub(super) fn clear(&self, cx: &mut Context<'_, Settings>) {
        self.name
            .update(cx, |field, cx| field.set(String::new(), cx));
    }

    pub(super) fn theme(&self, theme: Theme, cx: &mut Context<'_, Settings>) {
        self.name.update(cx, |field, _| field.theme(theme));
    }
}

#[derive(Clone)]
enum Edit {
    Capture,
    Cancel,
    Load(SharedString),
    Replace(SharedString),
    Remove(SharedString),
}

impl Settings {
    fn presets_busy(&self) -> bool {
        self.setup.is_some()
            || self.retiring_setup.is_some()
            || self.file_picker.is_some()
            || self.saving.is_some()
    }

    pub(super) fn capture_preset(
        &mut self,
        replacing: Option<&str>,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if cx.global::<Services>().is_quitting() {
            return false;
        }
        if self.setup.is_some() || self.retiring_setup.is_some() || self.file_picker.is_some() {
            self.notice = Some(Notice::Note(
                "Finish model setup or file selection before storing a preset.".into(),
            ));
            cx.notify();
            return false;
        }
        let replacing = match replacing {
            Some(name) => {
                let Some(index) = self
                    .config
                    .presets
                    .iter()
                    .position(|preset| preset.name() == name)
                else {
                    return false;
                };
                Some(index)
            },
            None => None,
        };
        if (self.vocabulary.pending(cx) && !self.add_correction(cx))
            || (self.recognition.pending(cx) && !self.add_recognition_term(cx))
        {
            return false;
        }
        let name = if let Some(index) = replacing {
            let Some(preset) = self.config.presets.get(index) else {
                return false;
            };
            preset.name().to_owned()
        } else {
            self.presets.name.read(cx).text().to_owned()
        };
        let result = (|| {
            let preset = Preset::capture(name, &self.config)?;
            let mut presets = self.config.presets.clone();
            if let Some(current) = replacing.and_then(|index| presets.get_mut(index)) {
                *current = preset;
            } else {
                presets.push(preset);
            }
            presets::validate(&presets)?;
            anyhow::Ok(presets)
        })();
        match result {
            Ok(presets) => {
                self.config.presets = presets;
                self.presets.clear(cx);
                self.edited();
                self.notice = Some(Notice::Note(
                    "Preset stored in the draft. Save changes to keep it.".into(),
                ));
                cx.notify();
                true
            },
            Err(error) => {
                self.notice = Some(Notice::Problem(format!("{error:#}").into()));
                cx.notify();
                false
            },
        }
    }

    fn edit_preset(&mut self, action: Edit, window: &mut Window, cx: &mut Context<'_, Self>) {
        if cx.global::<Services>().is_quitting() || self.presets_busy() {
            return;
        }
        if matches!(action, Edit::Load(_) | Edit::Replace(_)) && self.presets.pending(cx) {
            self.notice = Some(Notice::Note(
                "Store or cancel the new preset name first.".into(),
            ));
            cx.notify();
            return;
        }
        match action {
            Edit::Capture => {
                self.capture_preset(None, cx);
            },
            Edit::Cancel => self.presets.clear(cx),
            Edit::Load(name) => {
                if self.vocabulary.pending(cx) || self.recognition.pending(cx) {
                    self.notice = Some(Notice::Note("Finish or cancel the phrase and recognition-hint edits before loading a preset.".into()));
                    cx.notify();
                    return;
                }
                let Some(preset) = self
                    .config
                    .presets
                    .iter()
                    .find(|preset| preset.name() == name.as_ref())
                    .cloned()
                else {
                    return;
                };
                self.close_language(window);
                preset.load(&mut self.config);
                self.edited();
                self.notice = Some(Notice::Note(
                    "Preset loaded into the draft. Save changes to apply it.".into(),
                ));
            },
            Edit::Replace(name) => {
                self.capture_preset(Some(name.as_ref()), cx);
            },
            Edit::Remove(name) => {
                if let Some(index) = self
                    .config
                    .presets
                    .iter()
                    .position(|preset| preset.name() == name.as_ref())
                {
                    self.config.presets.remove(index);
                    self.edited();
                }
            },
        }
        cx.notify();
    }

    pub(super) fn presets_section(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let palette = self.config.theme.palette();
        let busy = self.presets_busy();
        let button = |id: SharedString, label: &'static str, action: Edit| {
            let accessible: SharedString = match &action {
                Edit::Load(name) | Edit::Replace(name) | Edit::Remove(name) => {
                    format!("{label} preset {name}").into()
                },
                _ => label.into(),
            };
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(accessible)
                .aria_disabled(busy)
                .focusable()
                .tab_stop(!busy)
                .cursor_pointer()
                .px(px(10.0))
                .py(px(7.0))
                .border_1()
                .border_color(alpha(palette.lamp, 0.25))
                .rounded(px(4.0))
                .focus(|style| style.border_color(rgb(palette.lamp)))
                .when(busy, |button| button.opacity(0.45).cursor_default())
                .on_click(cx.listener(move |settings, _, window, cx| {
                    settings.edit_preset(action.clone(), window, cx);
                }))
                .child(label)
        };
        div().flex().flex_col().gap(px(8.0))
            .child("Dictation presets")
            .child(div().text_color(rgb(palette.muted)).child("Keep named models, languages, hints and corrections. Load a preset, then Save changes to apply it."))
            .child(div().flex().gap(px(8.0))
                .child(div().flex_1().min_w(px(0.0)).child(self.presets.name.clone()))
                .child(button("preset-capture".into(), "Store current", Edit::Capture))
                .when(self.presets.pending(cx), |row| row.child(button("preset-cancel".into(), "Cancel name", Edit::Cancel))))
            .child(div().id("preset-list").max_h(px(240.0)).overflow_y_scroll().flex().flex_col().gap(px(8.0))
                .children(self.config.presets.iter().map(|preset| {
                    let name: SharedString = preset.name().to_owned().into();
                    div().flex().flex_col().gap(px(4.0))
                        .child(preset.name().to_owned())
                        .child(div().text_color(rgb(palette.muted)).child(preset.description()))
                        .child(div().flex().flex_wrap().gap(px(8.0))
                            .child(button(format!("preset-load-{name}").into(), "Load", Edit::Load(name.clone())))
                            .child(button(format!("preset-replace-{name}").into(), "Replace from draft", Edit::Replace(name.clone())))
                            .child(button(format!("preset-remove-{name}").into(), "Delete", Edit::Remove(name))))
                })))
    }
}

#[cfg(test)]
pub(super) mod native_tests;
