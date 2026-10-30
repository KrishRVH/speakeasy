//! Phrase corrections edit the existing Settings draft and use its durable save queue.

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, SharedString, Window, div, prelude::*, px,
    rgb,
};

use super::{
    Notice, Settings,
    text_field::{Submit, TextField},
};
use crate::{
    theme::{Theme, alpha},
    transcript::vocabulary::{self, Correction},
};

pub(super) struct Editor {
    heard: Entity<TextField>,
    written: Entity<TextField>,
    editing: Option<usize>,
}

impl Editor {
    pub(super) fn new(theme: Theme, cx: &mut Context<'_, Settings>) -> Self {
        let heard = cx.new(|cx| TextField::new(theme, cx));
        let written = cx.new(|cx| TextField::new(theme, cx));
        for field in [&heard, &written] {
            cx.observe(field, |_, _, cx| cx.notify()).detach();
            cx.subscribe(field, |settings, _, _: &Submit, cx| {
                settings.add_correction(cx);
            })
            .detach();
        }
        Self {
            heard,
            written,
            editing: None,
        }
    }

    pub(super) fn pending(&self, cx: &App) -> bool {
        self.editing.is_some()
            || !self.heard.read(cx).text().is_empty()
            || !self.written.read(cx).text().is_empty()
    }

    pub(super) fn theme(&self, theme: Theme, cx: &mut Context<'_, Settings>) {
        for field in [&self.heard, &self.written] {
            field.update(cx, |field, _| field.theme(theme));
        }
    }

    fn clear(&mut self, cx: &mut Context<'_, Settings>) {
        self.editing = None;
        for field in [&self.heard, &self.written] {
            field.update(cx, |field, cx| field.set(String::new(), cx));
        }
    }
}

#[derive(Clone, Copy)]
enum Edit {
    Add,
    Cancel,
    Modify(usize),
    Remove(usize),
}

impl Settings {
    pub(super) fn add_correction(&mut self, cx: &mut Context<'_, Self>) -> bool {
        let correction = Correction {
            heard: self.vocabulary.heard.read(cx).text().trim().to_owned(),
            written: self.vocabulary.written.read(cx).text().trim().to_owned(),
        };
        let mut rules = self.config.vocabulary.clone();
        if let Some(rule) = self
            .vocabulary
            .editing
            .and_then(|index| rules.get_mut(index))
        {
            *rule = correction;
        } else {
            rules.push(correction);
        }
        if let Err(error) = vocabulary::validate(&rules) {
            self.notice = Some(Notice::Problem(error.to_string().into()));
            cx.notify();
            return false;
        }
        self.config.vocabulary = rules;
        self.vocabulary.clear(cx);
        self.edited();
        cx.notify();
        true
    }

    fn edit_correction(&mut self, action: Edit, _: &mut Window, cx: &mut Context<'_, Self>) {
        if cx.global::<super::Services>().is_quitting() {
            return;
        }
        match action {
            Edit::Add => {
                self.add_correction(cx);
            },
            Edit::Cancel => self.vocabulary.clear(cx),
            Edit::Modify(index) => {
                if let Some(rule) = self.config.vocabulary.get(index) {
                    self.vocabulary
                        .heard
                        .update(cx, |field, cx| field.set(rule.heard.clone(), cx));
                    self.vocabulary
                        .written
                        .update(cx, |field, cx| field.set(rule.written.clone(), cx));
                    self.vocabulary.editing = Some(index);
                }
            },
            Edit::Remove(index) => {
                if index < self.config.vocabulary.len() {
                    self.config.vocabulary.remove(index);
                    self.vocabulary.editing = match self.vocabulary.editing {
                        Some(editing) if editing == index => {
                            self.vocabulary.clear(cx);
                            None
                        },
                        Some(editing) if editing > index => Some(editing.saturating_sub(1)),
                        editing => editing,
                    };
                    self.edited();
                }
            },
        }
        cx.notify();
    }

    pub(super) fn vocabulary_section(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let palette = self.config.theme.palette();
        let button = |id: SharedString, label: &'static str, action: Edit| {
            div()
                .id(id)
                .focusable()
                .tab_stop(true)
                .cursor_pointer()
                .px(px(10.0))
                .py(px(7.0))
                .border_1()
                .border_color(alpha(palette.lamp, 0.25))
                .rounded(px(4.0))
                .focus(|style| style.border_color(rgb(palette.lamp)))
                .on_click(cx.listener(move |settings, _, window, cx| {
                    settings.edit_correction(action, window, cx);
                }))
                .child(label)
        };
        let add_label = if self.vocabulary.editing.is_some() {
            "Update phrase"
        } else {
            "Add phrase"
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().child("Phrase corrections"))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(palette.muted))
                    .child("Replace recurring misheard phrases locally in dictation and Show narration. Whole phrases only; spelling and capitalization follow what you write."),
            )
            .child(
                div()
                    .flex()
                    .gap(px(12.0))
                    .child(div().flex_1().min_w(px(0.0)).child("When I say").child(self.vocabulary.heard.clone()))
                    .child(div().flex_1().min_w(px(0.0)).child("Write").child(self.vocabulary.written.clone())),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(button("correction-add".into(), add_label, Edit::Add))
                    .when(self.vocabulary.pending(cx), |row| {
                        row.child(button("correction-cancel".into(), "Cancel edit", Edit::Cancel))
                    }),
            )
            .child(
                div()
                    .id("correction-list")
                    .max_h(px(200.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .children(self.config.vocabulary.iter().enumerate().map(|(index, rule)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().flex_1().min_w(px(0.0)).child(format!("{} → {}", rule.heard, rule.written)))
                            .child(button(format!("correction-edit-{index}").into(), "Edit", Edit::Modify(index)))
                            .child(button(format!("correction-remove-{index}").into(), "Remove", Edit::Remove(index)))
                    })),
            )
    }
}
