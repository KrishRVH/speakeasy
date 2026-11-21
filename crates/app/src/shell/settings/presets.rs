use gpui::{
    AppContext as _, Context, Div, Entity, Focusable as _, FontWeight, Window, div, prelude::*, px,
    rgb,
};

use super::{
    Action, Notice, Services, Settings,
    menu::Menu,
    render::Ui,
    text_field::{Submit, TextField},
};
use crate::{
    config::{
        Config,
        presets::{self, LIMIT, Preset},
    },
    controls::{Weight, icon},
    gpui_ext::EntityUpdate as _,
    icons,
    shell::choices::Used,
    theme::Theme,
};

pub(super) struct Editor {
    name: Entity<TextField>,
    naming: bool,
    resolved: Option<(Preset, Preset)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edit {
    Use(usize),
    Update(usize),
    Delete(usize),
    Name,
    Save,
    Cancel,
}

impl Editor {
    pub(super) fn new(theme: Theme, window: &mut Window, cx: &mut Context<'_, Settings>) -> Self {
        let name = cx.new(|cx| {
            TextField::new("Preset name", theme, window, cx).placeholder("Name this preset")
        });
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&name, window, |settings, _, _: &Submit, window, cx| {
            settings.edit_preset(Edit::Save, window, cx);
        })
        .detach();
        Self {
            name,
            naming: false,
            resolved: None,
        }
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
    pub(super) fn enter(&mut self, name: &str, cx: &mut Context<'_, Settings>) {
        self.naming = true;
        self.name
            .update(cx, |field, cx| field.set_text(name.into(), cx));
    }

    pub(super) fn clear(&mut self, cx: &mut Context<'_, Settings>) {
        self.naming = false;
        self.name
            .update(cx, |field, cx| field.set_text(String::new(), cx));
    }

    pub(super) fn set_theme(&self, theme: Theme, cx: &mut Context<'_, Settings>) {
        self.name.update(cx, |field, _| field.set_theme(theme));
    }
}

impl Settings {
    pub(super) fn edit_preset(
        &mut self,
        edit: Edit,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let changes = matches!(edit, Edit::Use(_) | Edit::Update(_) | Edit::Save);
        if cx.global::<Services>().is_quitting()
            || (changes && self.engine_work.is_busy())
            || (matches!(edit, Edit::Use(_)) && self.using.is_some())
        {
            return;
        }
        match edit {
            Edit::Use(index) => {
                if let Some(preset) = self.config.presets.get(index).cloned() {
                    let mut selected = self.config.clone();
                    preset.load(&mut selected);
                    if !selected.speech_configured() {
                        self.notice = Some(Notice::Problem(
                            "Choose an engine and model, then update this preset.".into(),
                        ));
                        cx.notify();
                        return;
                    }
                    self.use_preset(preset, selected, cx);
                }
            },
            Edit::Update(index) => {
                if let Some(name) = self
                    .config
                    .presets
                    .get(index)
                    .map(|preset| preset.name().to_owned())
                {
                    self.store_preset(&name, Some(index), cx);
                }
            },
            Edit::Delete(index) => {
                if index < self.config.presets.len() {
                    self.config.presets.remove(index);
                    self.edited(cx);
                }
            },
            Edit::Name => {
                self.presets.naming = true;
                self.presets
                    .name
                    .read(cx)
                    .focus_handle(cx)
                    .focus(window, cx);
            },
            Edit::Save => {
                let name = self.presets.name.read(cx).text().to_owned();
                if self.store_preset(&name, None, cx) {
                    self.presets.clear(cx);
                }
            },
            Edit::Cancel => self.presets.clear(cx),
        }
        cx.notify();
    }

    fn use_preset(&mut self, preset: Preset, selected: Config, cx: &mut Context<'_, Self>) {
        if self.locked() || cx.global::<Services>().mode.is_demo() {
            self.load_preset(&preset, cx);
            return;
        }
        match cx.update_global::<Services, _>(|services, cx| services.use_preset(selected, cx)) {
            Ok(used) => {
                self.using = Some(cx.spawn(async move |this, cx| {
                    let used = used.recv().await.ok();
                    this.update_if_alive(cx, |view, cx| view.finish_use(&preset, used, cx));
                }));
            },
            Err(reason) => self.notice = Some(Notice::Problem(reason.into())),
        }
    }

    fn finish_use(&mut self, preset: &Preset, used: Option<Used>, cx: &mut Context<'_, Self>) {
        self.using = None;
        match used {
            Some(Ok(config)) => {
                let resolved = Preset::capture(preset.name(), &config)
                    .unwrap_or_else(|_unchanged| preset.clone());
                self.presets.resolved = Some((preset.clone(), resolved.clone()));
                self.load_preset(&resolved, cx);
            },
            Some(Err(reason)) => self.notice = Some(Notice::Problem(reason.into())),
            None => (),
        }
        cx.notify();
    }

    fn load_preset(&mut self, preset: &Preset, cx: &mut Context<'_, Self>) {
        preset.load(&mut self.config);
        self.own_engine = false;
        self.choosing_model = false;
        self.vocabulary.stop_revising();
        self.edited(cx);
    }

    fn store_preset(
        &mut self,
        name: &str,
        replacing: Option<usize>,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let stored = Preset::capture(name, &self.config).and_then(|preset| {
            let mut presets = self.config.presets.clone();
            match replacing.and_then(|index| presets.get_mut(index)) {
                Some(current) => *current = preset,
                None => presets.push(preset),
            }
            presets::validate(&presets)?;
            Ok(presets)
        });
        match stored {
            Ok(presets) => {
                self.config.presets = presets;
                self.edited(cx);
                true
            },
            Err(error) => {
                self.notice = Some(Notice::Problem(format!("{error:#}").into()));
                false
            },
        }
    }

    fn preset_in_use(&self, preset: &Preset) -> bool {
        self.presentation.preset.as_deref().map_or_else(
            || {
                preset.matches(&self.config)
                    || self
                        .presets
                        .resolved
                        .as_ref()
                        .is_some_and(|(stored, resolved)| {
                            stored == preset && resolved.matches(&self.config)
                        })
            },
            |active| active == preset.name(),
        )
    }

    pub(super) fn presets_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let busy = self.engine_work.is_busy();
        let mut rows: Vec<_> = self
            .config
            .presets
            .iter()
            .enumerate()
            .map(|(index, preset)| {
                let name = preset.name().to_owned();
                let menu = Menu::Preset(index);
                let more = kit.icon_button(
                    Action::Menu(menu).element_id(),
                    icons::MORE,
                    format!("More for {name}"),
                    Some(ui.press(Action::Menu(menu))),
                );
                let status = if self.preset_in_use(preset) {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .px(px(6.0))
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(palette.lamp))
                        .child(icon(icons::CHECK, 14.0, palette.lamp))
                        .child("In use")
                        .into_any_element()
                } else {
                    ui.button_if(
                        "Use",
                        Action::Preset(Edit::Use(index)),
                        Weight::Secondary,
                        !busy && !self.presentation.in_session && self.using.is_none(),
                    )
                    .aria_label(format!("Use {name}"))
                    .into_any_element()
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .min_h(px(56.0))
                    .pl(px(16.0))
                    .pr(px(8.0))
                    .py(px(8.0))
                    .child(kit.label(name, preset.description()).flex_1())
                    .child(status)
                    .child(self.anchor_menu(ui, menu, "Preset actions", more, true))
                    .into_any_element()
            })
            .collect();
        let full = self.config.presets.len() >= LIMIT;
        rows.push(if self.presets.naming {
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .pl(px(16.0))
                .pr(px(10.0))
                .py(px(10.0))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(self.presets.name.clone()),
                )
                .child(ui.button_if(
                    "Save",
                    Action::Preset(Edit::Save),
                    Weight::Primary,
                    !busy
                        && self.config.speech_configured()
                        && !self.presets.name.read(ui.cx).text().trim().is_empty(),
                ))
                .child(ui.button("Cancel", Action::Preset(Edit::Cancel), Weight::Quiet))
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .p(px(8.0))
                .child(
                    ui.button_if(
                        "Save current settings as a preset",
                        Action::Preset(Edit::Name),
                        Weight::Quiet,
                        !busy && !full && self.config.speech_configured(),
                    )
                    .pl(px(8.0))
                    .pr(px(10.0))
                    .child(icon(icons::PLUS, 14.0, palette.muted))
                    .flex_row_reverse(),
                )
                .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(kit.heading("Presets"))
                    .child(ui.text(
                        "A preset keeps a speech model with its language, GPU use, hints, corrections, and um and uh removal.",
                    )),
            )
            .child(kit.group(rows))
            .when(full, |section| {
                section.child(ui.text(format!(
                    "Speakeasy keeps up to {LIMIT} presets. Delete one to save another."
                )))
            })
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
pub(super) mod native_tests;
