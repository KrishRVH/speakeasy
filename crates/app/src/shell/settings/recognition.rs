use gpui::{AppContext as _, Context, Div, Entity, Focusable as _, Window, div, prelude::*, px};

use super::{
    Action, Notice, Settings,
    render::Ui,
    speech::Speech,
    text_field::{Submit, TextField},
};
use crate::{
    controls::{Weight, icon},
    icons,
    models::Model,
    shell::Services,
    theme::Theme,
};

pub(super) struct Editor {
    field: Entity<TextField>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edit {
    Add,
    Remove(usize),
}

impl Editor {
    pub(super) fn clear(&self, cx: &mut Context<'_, Settings>) {
        self.field
            .update(cx, |field, cx| field.set(String::new(), cx));
    }

    pub(super) fn new(theme: Theme, window: &Window, cx: &mut Context<'_, Settings>) -> Self {
        let field = cx.new(|cx| {
            TextField::new("New recognition hint", theme, cx).placeholder("Add a name or term")
        });
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&field, window, |settings, _, _: &Submit, window, cx| {
            settings.edit_hint(Edit::Add, window, cx);
        })
        .detach();
        Self { field }
    }

    pub(super) fn theme(&self, theme: Theme, cx: &mut Context<'_, Settings>) {
        self.field.update(cx, |field, _| field.theme(theme));
    }
}

impl Settings {
    pub(super) fn edit_hint(
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
                let added = self
                    .config
                    .recognition_terms
                    .add(self.recognition.field.read(cx).text());
                match added {
                    Ok(()) => {
                        self.recognition.clear(cx);
                        self.edited(cx);
                    },
                    Err(error) => self.notice = Some(Notice::Problem(error.to_string().into())),
                }
            },
            Edit::Remove(index) => {
                if index < self.config.recognition_terms.phrases().len() {
                    self.config.recognition_terms.remove(index);
                    self.edited(cx);
                }
            },
        }
        self.recognition
            .field
            .read(cx)
            .focus_handle(cx)
            .focus(window);
        cx.notify();
    }

    pub(super) fn shows_hints(&self) -> bool {
        self.config.uses_nemotron() || !self.config.recognition_terms.phrases().is_empty()
    }

    pub(super) fn recognition_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let palette = ui.palette;
        let phrases = self.config.recognition_terms.phrases();
        let entry = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(self.recognition.field.clone()),
            )
            .child(ui.button_if(
                "Add",
                Action::Hint(Edit::Add),
                Weight::Secondary,
                !self.recognition.field.read(ui.cx).text().trim().is_empty(),
            ));
        let chips = div()
            .flex()
            .flex_wrap()
            .gap(px(6.0))
            .children(phrases.iter().enumerate().map(|(index, phrase)| {
                let remove = Action::Hint(Edit::Remove(index));
                kit.chip(
                    phrase.clone(),
                    remove.element_id(),
                    format!("Remove hint {phrase}"),
                    ui.press(remove),
                )
            }));
        let unused = (!self.config.uses_nemotron()).then(|| {
            let model = match Speech::of(&self.config, self.own_engine) {
                Some(Speech::Managed(Model::Parakeet)) => "Parakeet",
                _ => "your own engine",
            };
            div()
                .flex()
                .items_start()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(8.0))
                .bg(palette.hover())
                .child(div().pt(px(2.0)).child(icon(icons::INFO, 14.0, palette.muted)))
                .child(kit.detail(format!(
                    "Only Nemotron 3.5 uses hints, so {model} ignores these. Corrections still apply."
                )))
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
                    .child(kit.heading("Recognition hints"))
                    .child(ui.text(
                        "Names and terms Nemotron should listen for. Hints help it hear them; they don't force a spelling.",
                    )),
            )
            .children(unused)
            .child(
                kit.group(vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.0))
                        .p(px(16.0))
                        .child(entry)
                        .when(!phrases.is_empty(), |body| body.child(chips))
                        .into_any_element(),
                ]),
            )
    }
}
