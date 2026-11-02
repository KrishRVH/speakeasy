use gpui::{
    App, AppContext as _, Context, Div, Entity, Focusable as _, Window, div, prelude::*, px, rgb,
};

use super::{
    Action, Notice, Settings,
    render::Ui,
    text_field::{Submit, TextField},
};
use crate::{
    controls::{Weight, icon},
    icons,
    shell::Services,
    theme::Theme,
    transcript::vocabulary::{self, Correction},
};

pub(super) struct Editor {
    heard: Entity<TextField>,
    written: Entity<TextField>,
    revised_heard: Entity<TextField>,
    revised_written: Entity<TextField>,
    editing: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edit {
    Add,
    Revise(usize),
    Save,
    Cancel,
    Remove(usize),
}

impl Editor {
    pub(super) fn new(theme: Theme, window: &Window, cx: &mut Context<'_, Settings>) -> Self {
        let mut field = |label: &'static str, placeholder: &'static str| {
            cx.new(|cx| TextField::new(label, theme, cx).placeholder(placeholder))
        };
        let heard = field("When I say", "speak easy");
        let written = field("Write", "Speakeasy");
        let revised_heard = field("When I say", "");
        let revised_written = field("Write", "");
        for (field, edit) in [
            (&heard, Edit::Add),
            (&written, Edit::Add),
            (&revised_heard, Edit::Save),
            (&revised_written, Edit::Save),
        ] {
            cx.observe(field, |_, _, cx| cx.notify()).detach();
            cx.subscribe_in(field, window, move |settings, _, _: &Submit, window, cx| {
                settings.edit_correction(edit, window, cx);
            })
            .detach();
        }
        Self {
            heard,
            written,
            revised_heard,
            revised_written,
            editing: None,
        }
    }

    pub(super) fn theme(&self, theme: Theme, cx: &mut Context<'_, Settings>) {
        for field in self.fields() {
            field.update(cx, |field, _| field.theme(theme));
        }
    }

    pub(super) fn clear(&mut self, cx: &mut Context<'_, Settings>) {
        self.editing = None;
        for field in self.fields() {
            field.update(cx, |field, cx| field.set(String::new(), cx));
        }
    }

    pub(super) const fn stop_revising(&mut self) {
        self.editing = None;
    }

    const fn fields(&self) -> [&Entity<TextField>; 4] {
        [
            &self.heard,
            &self.written,
            &self.revised_heard,
            &self.revised_written,
        ]
    }

    fn draft(&self, revising: bool, cx: &App) -> Correction {
        let (heard, written) = if revising {
            (&self.revised_heard, &self.revised_written)
        } else {
            (&self.heard, &self.written)
        };
        Correction {
            heard: heard.read(cx).text().trim().to_owned(),
            written: written.read(cx).text().trim().to_owned(),
        }
    }
}

impl Settings {
    pub(super) fn edit_correction(
        &mut self,
        edit: Edit,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if cx.global::<Services>().is_quitting() {
            return;
        }
        match edit {
            Edit::Add => {
                if self.store_correction(None, cx) {
                    for field in [&self.vocabulary.heard, &self.vocabulary.written] {
                        field.update(cx, |field, cx| field.set(String::new(), cx));
                    }
                    self.vocabulary
                        .heard
                        .read(cx)
                        .focus_handle(cx)
                        .focus(window, cx);
                }
            },
            Edit::Revise(index) => {
                if let Some(rule) = self.config.vocabulary.get(index) {
                    let (heard, written) = (rule.heard.clone(), rule.written.clone());
                    self.vocabulary
                        .revised_heard
                        .update(cx, |field, cx| field.set(heard, cx));
                    self.vocabulary
                        .revised_written
                        .update(cx, |field, cx| field.set(written, cx));
                    self.vocabulary.editing = Some(index);
                    self.vocabulary
                        .revised_heard
                        .read(cx)
                        .focus_handle(cx)
                        .focus(window, cx);
                }
            },
            Edit::Save => {
                if let Some(index) = self.vocabulary.editing
                    && self.store_correction(Some(index), cx)
                {
                    self.vocabulary.editing = None;
                }
            },
            Edit::Cancel => self.vocabulary.editing = None,
            Edit::Remove(index) => {
                if index < self.config.vocabulary.len() {
                    self.config.vocabulary.remove(index);
                    self.vocabulary.editing = match self.vocabulary.editing {
                        Some(editing) if editing == index => None,
                        Some(editing) if editing > index => Some(editing.saturating_sub(1)),
                        editing => editing,
                    };
                    self.edited(cx);
                }
            },
        }
        cx.notify();
    }

    fn store_correction(&mut self, replacing: Option<usize>, cx: &mut Context<'_, Self>) -> bool {
        let correction = self.vocabulary.draft(replacing.is_some(), cx);
        let mut rules = self.config.vocabulary.clone();
        match replacing {
            Some(index) => match rules.get_mut(index) {
                Some(rule) => *rule = correction,
                None => return false,
            },
            None => rules.push(correction),
        }
        if let Err(error) = vocabulary::validate(&rules) {
            self.notice = Some(Notice::Problem(error.to_string().into()));
            return false;
        }
        self.config.vocabulary = rules;
        self.edited(cx);
        true
    }

    pub(super) fn vocabulary_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let arrow = || icon(icons::ARROW_RIGHT, 14.0, palette.muted).flex_none();
        let complete =
            |correction: Correction| !correction.heard.is_empty() && !correction.written.is_empty();
        let labeled = |label: &'static str, field: &Entity<TextField>| {
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(kit.detail(label))
                .child(field.clone())
        };
        let add = div()
            .flex()
            .items_end()
            .gap(px(10.0))
            .p(px(16.0))
            .child(labeled("When I say", &self.vocabulary.heard))
            .child(div().h(px(32.0)).flex().items_center().child(arrow()))
            .child(labeled("Write", &self.vocabulary.written))
            .child(ui.button_if(
                "Add",
                Action::Correction(Edit::Add),
                Weight::Primary,
                complete(self.vocabulary.draft(false, ui.cx)),
            ));
        let mut rows = vec![add.into_any_element()];
        if self.config.vocabulary.is_empty() {
            rows.push(
                kit.detail(
                    "No corrections yet. Add a phrase Speakeasy gets wrong and what to write instead.",
                )
                .px(px(16.0))
                .py(px(14.0))
                .into_any_element(),
            );
        }
        rows.extend(
            self.config
                .vocabulary
                .iter()
                .enumerate()
                .map(|(index, rule)| {
                    if self.vocabulary.editing == Some(index) {
                        return div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .px(px(16.0))
                            .py(px(10.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(self.vocabulary.revised_heard.clone()),
                            )
                            .child(arrow())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(self.vocabulary.revised_written.clone()),
                            )
                            .child(ui.button_if(
                                "Save",
                                Action::Correction(Edit::Save),
                                Weight::Primary,
                                complete(self.vocabulary.draft(true, ui.cx)),
                            ))
                            .child(ui.button(
                                "Cancel",
                                Action::Correction(Edit::Cancel),
                                Weight::Quiet,
                            ))
                            .into_any_element();
                    }
                    let remove = Action::Correction(Edit::Remove(index));
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .min_h(px(48.0))
                        .pl(px(16.0))
                        .pr(px(8.0))
                        .py(px(6.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(10.0))
                                .child(
                                    div()
                                        .text_color(rgb(palette.muted))
                                        .child(rule.heard.clone()),
                                )
                                .child(arrow())
                                .child(rule.written.clone()),
                        )
                        .child(
                            ui.button(
                                "Edit",
                                Action::Correction(Edit::Revise(index)),
                                Weight::Quiet,
                            )
                            .aria_label(format!("Edit correction {}", rule.heard)),
                        )
                        .child(kit.icon_button(
                            remove.element_id(),
                            icons::CLOSE,
                            format!("Remove correction {}", rule.heard),
                            Some(ui.press(remove)),
                        ))
                        .into_any_element()
                }),
        );
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(kit.heading("Corrections"))
                    .child(ui.text("Fix what Speakeasy mishears. Works with every speech model.")),
            )
            .child(kit.group(rows))
    }
}
