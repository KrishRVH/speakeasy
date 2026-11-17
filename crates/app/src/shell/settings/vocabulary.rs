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
    addition: Pair,
    revision: Pair,
    editing: Option<usize>,
    practiced: Entity<TextField>,
}

struct Pair {
    heard: Entity<TextField>,
    written: Entity<TextField>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edit {
    Add,
    Revise(usize),
    Save,
    Cancel,
    Remove(usize),
    FromPractice(usize),
    AddFromPractice,
}

pub(super) fn practice_words(text: &str) -> impl Iterator<Item = &str> {
    text.split_whitespace()
        .map(|token| token.trim_matches(|character: char| !character.is_alphanumeric()))
        .filter(|word| !word.is_empty())
}

fn pick(picked: Option<std::ops::Range<usize>>, word: usize) -> std::ops::Range<usize> {
    match picked {
        Some(range) if range.end == word => range.start..word.saturating_add(1),
        Some(range) if word.checked_add(1) == Some(range.start) => word..range.end,
        Some(_) | None => word..word.saturating_add(1),
    }
}

impl Editor {
    pub(super) fn new(theme: Theme, window: &Window, cx: &mut Context<'_, Settings>) -> Self {
        Self {
            addition: Pair::new(["speak easy", "Speakeasy"], Edit::Add, theme, window, cx),
            revision: Pair::new(["", ""], Edit::Save, theme, window, cx),
            editing: None,
            practiced: field(
                "Write",
                "What it should say",
                Edit::AddFromPractice,
                theme,
                window,
                cx,
            ),
        }
    }

    pub(super) fn set_theme(&self, theme: Theme, cx: &mut Context<'_, Settings>) {
        for field in [&self.addition, &self.revision]
            .into_iter()
            .flat_map(Pair::fields)
            .chain([&self.practiced])
        {
            field.update(cx, |field, _| field.set_theme(theme));
        }
    }

    pub(super) fn clear(&mut self, cx: &mut Context<'_, Settings>) {
        self.editing = None;
        for field in [&self.addition, &self.revision]
            .into_iter()
            .flat_map(Pair::fields)
            .chain([&self.practiced])
        {
            field.update(cx, |field, cx| field.set_text(String::new(), cx));
        }
    }

    #[cfg(test)]
    pub(super) fn practiced(&self) -> &Entity<TextField> {
        &self.practiced
    }

    #[cfg(test)]
    pub(super) fn addition_heard(&self, cx: &App) -> String {
        self.addition.heard.read(cx).text().to_owned()
    }

    pub(super) const fn stop_revising(&mut self) {
        self.editing = None;
    }
}

impl Pair {
    fn new(
        placeholders: [&'static str; 2],
        submit: Edit,
        theme: Theme,
        window: &Window,
        cx: &mut Context<'_, Settings>,
    ) -> Self {
        let [heard, written] = placeholders;
        Self {
            heard: field("When I say", heard, submit, theme, window, cx),
            written: field("Write", written, submit, theme, window, cx),
        }
    }

    const fn fields(&self) -> [&Entity<TextField>; 2] {
        [&self.heard, &self.written]
    }

    fn fill(&self, Correction { heard, written }: Correction, cx: &mut Context<'_, Settings>) {
        self.heard.update(cx, |field, cx| field.set_text(heard, cx));
        self.written
            .update(cx, |field, cx| field.set_text(written, cx));
    }

    fn clear(&self, cx: &mut Context<'_, Settings>) {
        for field in self.fields() {
            field.update(cx, |field, cx| field.set_text(String::new(), cx));
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut Context<'_, Settings>) {
        self.heard.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn draft(&self, cx: &App) -> Correction {
        let text = |field: &Entity<TextField>| field.read(cx).text().trim().to_owned();
        Correction {
            heard: text(&self.heard),
            written: text(&self.written),
        }
    }

    fn is_complete(&self, cx: &App) -> bool {
        self.fields()
            .into_iter()
            .all(|field| !field.read(cx).text().trim().is_empty())
    }
}

fn field(
    label: &'static str,
    placeholder: &'static str,
    submit: Edit,
    theme: Theme,
    window: &Window,
    cx: &mut Context<'_, Settings>,
) -> Entity<TextField> {
    let field = cx.new(|cx| TextField::new(label, theme, cx).placeholder(placeholder));
    cx.observe(&field, |_, _, cx| cx.notify()).detach();
    cx.subscribe_in(
        &field,
        window,
        move |settings, _, _: &Submit, window, cx| {
            settings.edit_correction(submit, window, cx);
        },
    )
    .detach();
    field
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
                if self.store_correction(self.vocabulary.addition.draft(cx), None, cx) {
                    self.vocabulary.addition.clear(cx);
                    self.vocabulary.addition.focus(window, cx);
                }
            },
            Edit::Revise(index) => {
                if let Some(rule) = self.config.vocabulary.get(index) {
                    self.vocabulary.revision.fill(rule.clone(), cx);
                    self.vocabulary.editing = Some(index);
                    self.vocabulary.revision.focus(window, cx);
                }
            },
            Edit::Save => {
                if let Some(index) = self.vocabulary.editing
                    && self.store_correction(self.vocabulary.revision.draft(cx), Some(index), cx)
                {
                    self.vocabulary.editing = None;
                }
            },
            Edit::Cancel => self.vocabulary.editing = None,
            Edit::FromPractice(word) => {
                let range = pick(self.practice.picked.clone(), word);
                if range.end <= practice_words(&self.practice.text).count() {
                    if self.practice.picked.is_none() {
                        self.vocabulary
                            .practiced
                            .update(cx, |field, cx| field.set_text(String::new(), cx));
                    }
                    self.practice.picked = Some(range);
                    let practiced = &self.vocabulary.practiced;
                    practiced.read(cx).focus_handle(cx).focus(window, cx);
                }
            },
            Edit::AddFromPractice => {
                let written = self.vocabulary.practiced.read(cx).text().trim().to_owned();
                if let Some(heard) = self.practiced_phrase()
                    && !written.is_empty()
                    && self.store_correction(Correction { heard, written }, None, cx)
                {
                    self.vocabulary
                        .practiced
                        .update(cx, |field, cx| field.set_text(String::new(), cx));
                    self.notice = Some(Notice::Note(
                        "Correction added. Practice again to hear it.".into(),
                    ));
                    self.navigation
                        .tab(super::navigation::Page::Dictation)
                        .focus(window, cx);
                }
            },
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

    fn store_correction(
        &mut self,
        correction: Correction,
        replacing: Option<usize>,
        cx: &mut Context<'_, Self>,
    ) -> bool {
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

    pub(super) fn practiced_phrase(&self) -> Option<String> {
        let range = self.practice.picked.clone()?;
        let words: Vec<&str> = practice_words(&self.practice.text).collect();
        words.get(range).map(|words| words.join(" "))
    }

    pub(super) fn picked_word(&self, word: usize) -> bool {
        self.practice
            .picked
            .as_ref()
            .is_some_and(|range| range.contains(&word))
    }

    pub(super) fn practice_correction(&self, ui: &Ui<'_>) -> Option<Div> {
        let heard = self.practiced_phrase()?;
        let kit = ui.kit();
        let palette = ui.palette;
        let written = &self.vocabulary.practiced;
        Some(
            div()
                .flex()
                .items_end()
                .gap(px(10.0))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(kit.detail("When I say"))
                        .child(
                            div()
                                .h(px(32.0))
                                .flex()
                                .items_center()
                                .text_color(rgb(palette.ink))
                                .child(heard),
                        ),
                )
                .child(
                    div()
                        .h(px(32.0))
                        .flex()
                        .items_center()
                        .child(icon(icons::ARROW_RIGHT, 14.0, palette.muted).flex_none()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(kit.detail("Write"))
                        .child(written.clone()),
                )
                .child(ui.button_if(
                    "Add",
                    Action::Correction(Edit::AddFromPractice),
                    Weight::Primary,
                    !written.read(ui.cx).text().trim().is_empty(),
                )),
        )
    }

    fn correction_row(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let arrow = || icon(icons::ARROW_RIGHT, 14.0, palette.muted).flex_none();
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
        div()
            .flex()
            .items_end()
            .gap(px(10.0))
            .child(labeled("When I say", &self.vocabulary.addition.heard))
            .child(div().h(px(32.0)).flex().items_center().child(arrow()))
            .child(labeled("Write", &self.vocabulary.addition.written))
            .child(ui.button_if(
                "Add",
                Action::Correction(Edit::Add),
                Weight::Primary,
                self.vocabulary.addition.is_complete(ui.cx),
            ))
    }

    pub(super) fn vocabulary_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let arrow = || icon(icons::ARROW_RIGHT, 14.0, palette.muted).flex_none();
        let mut rows = vec![self.correction_row(ui).p(px(16.0)).into_any_element()];
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
                                    .child(self.vocabulary.revision.heard.clone()),
                            )
                            .child(arrow())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(self.vocabulary.revision.written.clone()),
                            )
                            .child(ui.button_if(
                                "Save",
                                Action::Correction(Edit::Save),
                                Weight::Primary,
                                self.vocabulary.revision.is_complete(ui.cx),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn practice_words_drop_punctuation_and_adjacent_picks_grow_one_phrase() {
        let words: Vec<&str> = practice_words("Hello, speak easy! — it's 2 o'clock.").collect();
        assert_eq!(words, ["Hello", "speak", "easy", "it's", "2", "o'clock"]);
        let first = pick(None, 1);
        assert_eq!(first, 1..2);
        let grown = pick(Some(first), 2);
        assert_eq!(grown, 1..3);
        assert_eq!(pick(Some(grown.clone()), 0), 0..3);
        assert_eq!(pick(Some(grown), 5), 5..6);
    }
}
