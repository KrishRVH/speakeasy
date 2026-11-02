use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, div, prelude::*, px, rgb,
};

use super::{
    Notice, Settings,
    text_field::{Submit, TextField},
};
use crate::{
    controls::{Kit, Weight},
    shell::Services,
    theme::Theme,
};

pub(super) struct Editor {
    field: Entity<TextField>,
}

impl Editor {
    #[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
    pub(super) fn enter(&self, text: &str, cx: &mut Context<'_, Settings>) {
        self.field
            .update(cx, |field, cx| field.set(text.into(), cx));
    }

    pub(super) fn clear(&self, cx: &mut Context<'_, Settings>) {
        self.field
            .update(cx, |field, cx| field.set(String::new(), cx));
    }

    pub(super) fn new(theme: Theme, cx: &mut Context<'_, Settings>) -> Self {
        let field = cx.new(|cx| TextField::new("Recognition hints", theme, cx));
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&field, |settings, _, _: &Submit, cx| {
            settings.add_recognition_term(cx);
        })
        .detach();
        Self { field }
    }

    pub(super) fn pending(&self, cx: &App) -> bool {
        !self.field.read(cx).text().is_empty()
    }

    pub(super) fn theme(&self, theme: Theme, cx: &mut Context<'_, Settings>) {
        self.field.update(cx, |field, _| field.theme(theme));
    }
}

impl Settings {
    pub(super) fn add_recognition_term(&mut self, cx: &mut Context<'_, Self>) -> bool {
        if cx.global::<Services>().is_quitting() {
            return false;
        }
        if let Err(error) = self
            .config
            .recognition_terms
            .add(self.recognition.field.read(cx).text())
        {
            self.notice = Some(Notice::Problem(error.to_string().into()));
            cx.notify();
            return false;
        }
        self.recognition
            .field
            .update(cx, |field, cx| field.set(String::new(), cx));
        self.edited();
        cx.notify();
        true
    }

    pub(super) fn recognition_section(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let palette = self.config.theme.palette();
        let button = |id: gpui::SharedString, label: &'static str, remove: Option<usize>| {
            let accessible: gpui::SharedString = remove
                .and_then(|index| self.config.recognition_terms.phrases().get(index))
                .map_or_else(
                    || label.into(),
                    |phrase| format!("{label} hint {phrase}").into(),
                );
            Kit::new(palette)
                .button(
                    id,
                    label,
                    Weight::Secondary,
                    Some(cx.listener(move |settings, _, window, cx| {
                        if cx.global::<Services>().is_quitting() {
                            return;
                        }
                        match remove {
                            Some(index) => {
                                settings.config.recognition_terms.remove(index);
                                settings
                                    .recognition
                                    .field
                                    .read(cx)
                                    .focus_handle(cx)
                                    .focus(window);
                                settings.edited();
                                cx.notify();
                            },
                            None => {
                                settings.add_recognition_term(cx);
                            },
                        }
                    })),
                )
                .aria_label(accessible)
        };
        let detail = if self.config.uses_nemotron() {
            "Help Nemotron recognize names and jargon. Hints improve recognition; they do not force a spelling or replace words."
        } else {
            "Recognition hints are saved for Nemotron. The selected model does not use them; phrase corrections below still work."
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(Kit::new(palette).eyebrow("Recognition hints"))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(palette.muted))
                    .child(detail),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(self.recognition.field.clone()),
                    )
                    .child(button("recognition-add".into(), "Add hint", None)),
            )
            .child(
                div()
                    .id("recognition-list")
                    .max_h(px(140.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .children(
                        self.config
                            .recognition_terms
                            .phrases()
                            .iter()
                            .enumerate()
                            .map(|(index, phrase)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(div().flex_1().min_w(px(0.0)).child(phrase.clone()))
                                    .child(button(
                                        format!("recognition-remove-{index}").into(),
                                        "Remove",
                                        Some(index),
                                    ))
                            }),
                    ),
            )
    }
}
