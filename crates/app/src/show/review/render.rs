//! The Show library's native element tree. Private summary text is plain, inert text.

use std::time::SystemTime;

use gpui::{
    Context, Div, ElementId, FontWeight, KeyDownEvent, SharedString, Window, div, img, prelude::*,
    px, rgb,
};

use super::{Action, ImageState, Review, Source};
use crate::{
    BRAND_FONT,
    show::store::recall::Health,
    theme::{Palette, alpha, mix},
};

impl Render for Review {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let palette = self.theme.palette();
        let now = SystemTime::now();
        let selected = self.library.selected.as_ref();
        let sidebar = div()
            .id("show-library")
            .w(px(230.0))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(alpha(palette.lamp, 0.14))
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children(self.library.rows.iter().enumerate().map(|(index, row)| {
                let current = selected.is_some_and(|key| key.directory == row.directory);
                let description = row.duration.map_or_else(
                    || "Details unavailable".into(),
                    |duration| format!("{duration} · {} frames", row.frames),
                );
                button(Action::Show(index), palette, true, current, cx)
                    .w_full()
                    .py(px(14.0))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(5.0))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(row.recorded(now)),
                    )
                    .child(
                        div()
                            .text_color(rgb(palette.muted))
                            .text_size(px(11.0))
                            .child(description),
                    )
                    .when(row.pinned == Some(true), |row| {
                        row.child(tag("Kept", palette.lamp))
                    })
                    .when(row.silent, |row| {
                        row.child(tag("No narration", palette.warn))
                    })
                    .when(row.early, |row| row.child(tag("Ended early", palette.warn)))
                    .when(row.health != Health::Ok, |button| {
                        button.child(tag(
                            if row.health == Health::Unsupported {
                                "Can't open"
                            } else {
                                "Damaged"
                            },
                            palette.warn,
                        ))
                    })
            }));
        let content = if let Some(key) = selected {
            self.detail_pane(key, palette, now, cx).into_any_element()
        } else {
            let text = if self.library.gone {
                "This Show was deleted or has expired. Choose another Show to continue."
            } else if self.listing.is_some() {
                "Looking for Shows…"
            } else if !self.library.rows.is_empty() {
                "Choose a Show to review."
            } else if cfg!(target_os = "linux") {
                "No saved Shows. Linux opens saved Shows but doesn't record them yet."
            } else {
                "No saved Shows. While holding the dictation shortcut, press S, W, or A to record one."
            };
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(message(text, palette))
                .into_any_element()
        };
        div()
            .id("shows")
            .size_full()
            .bg(rgb(palette.panel))
            .text_color(rgb(palette.ink))
            .text_size(px(12.0))
            .flex()
            .flex_col()
            .on_key_down(cx.listener(Self::key_down))
            .track_focus(&self.focus)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(24.0))
                    .py(px(16.0))
                    .border_b_1()
                    .border_color(alpha(palette.lamp, 0.14))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(14.0))
                            .child(
                                div()
                                    .font_family(BRAND_FONT)
                                    .text_size(px(28.0))
                                    .child("Shows"),
                            )
                            .child(div().text_color(rgb(palette.muted)).child(
                                if self.source == Source::Demo {
                                    "Preview · synthetic recordings".into()
                                } else {
                                    format!("{} saved", self.library.rows.len())
                                },
                            )),
                    )
                    .child(
                        button(Action::Refresh, palette, self.listing.is_none(), false, cx)
                            .child("Refresh"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .child(sidebar)
                    .child(content),
            )
            .when_some(self.notice.clone(), |body, notice| {
                body.child(
                    div()
                        .px(px(24.0))
                        .py(px(10.0))
                        .border_t_1()
                        .border_color(alpha(palette.lamp, 0.14))
                        .child(notice),
                )
            })
    }
}

impl Review {
    fn detail_pane(
        &self,
        key: &super::library::Key,
        palette: &Palette,
        now: SystemTime,
        cx: &Context<'_, Self>,
    ) -> impl IntoElement {
        let row = self
            .library
            .rows
            .iter()
            .find(|row| row.directory == key.directory);
        let heading = row.map_or_else(
            || "Show".into(),
            |row| {
                let duration = row.duration.map_or_else(
                    || "Duration unavailable".into(),
                    |duration| duration.to_string(),
                );
                format!(
                    "{} · {duration} · {} keyframes · {} marks",
                    row.recorded(now),
                    row.frames,
                    row.marks
                )
            },
        );
        let retention = row.map_or_else(String::new, |row| row.retention(now));
        div()
            .id("show-detail")
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_y_scroll()
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().font_weight(FontWeight::MEDIUM).child(heading))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(palette.muted))
                    .child(retention),
            )
            .when_some(
                self.detail
                    .as_ref()
                    .filter(|detail| !detail.warnings.is_empty()),
                |body, detail| {
                    body.child(
                        div()
                            .id("show-warnings")
                            .max_h(px(78.0))
                            .overflow_y_scroll()
                            .text_size(px(11.0))
                            .text_color(rgb(palette.warn))
                            .children(
                                detail
                                    .warnings
                                    .iter()
                                    .map(|warning| div().child(warning.clone())),
                            ),
                    )
                },
            )
            .child(self.canvas(palette))
            .child(self.navigation(key.frame, palette, cx))
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(palette.muted))
                    .child("SUMMARY"),
            )
            .child(self.summary(palette))
            .child(self.commands(palette, cx))
    }

    fn canvas(&self, palette: &Palette) -> impl IntoElement {
        const UNAVAILABLE: &str =
            "Frame navigation is unavailable. The summary may still be readable.";
        div()
            .flex_1()
            .min_h(px(120.0))
            .overflow_hidden()
            .rounded(px(6.0))
            .bg(rgb(palette.room))
            .border_1()
            .border_color(alpha(palette.lamp, 0.12))
            .flex()
            .items_center()
            .justify_center()
            .child(match &self.image {
                ImageState::Ready(image) => img(std::sync::Arc::clone(image))
                    .size_full()
                    .into_any_element(),
                ImageState::Loading => message("Loading frame…", palette).into_any_element(),
                ImageState::Failed(error) => message(error.clone(), palette).into_any_element(),
                ImageState::Empty => message(
                    if self.opening.is_some() {
                        "Opening Show…"
                    } else {
                        UNAVAILABLE
                    },
                    palette,
                )
                .into_any_element(),
            })
    }

    fn navigation(
        &self,
        index: usize,
        palette: &Palette,
        cx: &Context<'_, Self>,
    ) -> impl IntoElement {
        let frames = self
            .detail
            .as_ref()
            .map_or(&[][..], |detail| detail.frames.as_slice());
        let count = frames.len();
        let caption = frames.get(index).map_or_else(
            || "No saved frame available".into(),
            |frame| {
                format!(
                    "#{} of {count} · {}{}",
                    frame.number, frame.at, frame.marker
                )
            },
        );
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .flex_none()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        button(Action::Previous, palette, index > 0 && count > 0, false, cx)
                            .child("‹"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(12.0))
                            .child(caption),
                    )
                    .child(
                        button(
                            Action::Next,
                            palette,
                            index.saturating_add(1) < count,
                            false,
                            cx,
                        )
                        .child("›"),
                    ),
            )
            .child(
                div()
                    .id("show-frame-numbers")
                    .overflow_x_scroll()
                    .flex()
                    .gap(px(5.0))
                    .flex_none()
                    .children(frames.iter().enumerate().map(|(position, frame)| {
                        button(
                            Action::Frame(position),
                            palette,
                            true,
                            index == position,
                            cx,
                        )
                        .flex_none()
                        .child(format!("#{} {}", frame.number, frame.at))
                    })),
            )
    }

    fn summary(&self, palette: &Palette) -> impl IntoElement {
        const REST: &str = "The rest is in summary.md. Copy summary includes the whole file.";
        let text = match self.detail.as_ref().map(|detail| &detail.summary) {
            Some(Ok(summary)) => {
                let end = summary.floor_char_boundary(64 << 10);
                div()
                    .child(summary.get(..end).unwrap_or_default().to_owned())
                    .when(end < summary.len(), |text| {
                        text.child(div().mt(px(8.0)).text_color(rgb(palette.warn)).child(REST))
                    })
            },
            Some(Err(error)) => div().text_color(rgb(palette.warn)).child(error.clone()),
            None => div().child(if self.opening.is_some() {
                "Loading summary…"
            } else {
                "Summary unavailable. Try Refresh or Open folder."
            }),
        };
        div()
            .id("show-summary")
            .h(px(150.0))
            .flex_none()
            .overflow_y_scroll()
            .p(px(12.0))
            .rounded(px(4.0))
            .bg(rgb(palette.room))
            .text_size(px(12.0))
            .line_height(px(19.0))
            .child(text)
    }

    fn commands(&self, palette: &Palette, cx: &Context<'_, Self>) -> impl IntoElement {
        let can_copy = self.source != Source::Demo && (self.can_copy)(cx) && self.copying.is_none();
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .when(!cfg!(target_os = "linux"), |bar| {
                bar.child(button(Action::Copy, palette, can_copy, false, cx).child("Copy summary"))
            })
            .child(button(Action::Folder, palette, true, false, cx).child("Open folder"))
    }
}

fn tag(label: &'static str, color: u32) -> impl IntoElement {
    div()
        .text_size(px(10.0))
        .text_color(rgb(color))
        .child(label)
}

fn message(text: impl Into<SharedString>, palette: &Palette) -> impl IntoElement {
    div()
        .p(px(24.0))
        .max_w(px(460.0))
        .text_color(rgb(palette.muted))
        .text_size(px(13.0))
        .child(text.into())
}

fn button(
    action: Action,
    palette: &Palette,
    enabled: bool,
    selected: bool,
    cx: &Context<'_, Review>,
) -> gpui::Stateful<Div> {
    let id: ElementId = match action {
        Action::Previous => "previous-frame".into(),
        Action::Next => "next-frame".into(),
        Action::Refresh => "shows-refresh".into(),
        Action::Copy => "shows-copy".into(),
        Action::Folder => "shows-folder".into(),
        Action::Show(index) => SharedString::from(format!("show-{index}")).into(),
        Action::Frame(index) => SharedString::from(format!("frame-{index}")).into(),
    };
    let accent = palette.lamp;
    let hover = rgb(mix(palette.raise, palette.ink, 0.08));
    div()
        .id(id)
        .focusable()
        .tab_stop(enabled)
        .px(px(10.0))
        .py(px(7.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(if selected {
            rgb(accent)
        } else {
            alpha(accent, 0.18)
        })
        .bg(rgb(if selected {
            mix(palette.raise, accent, 0.12)
        } else {
            palette.raise
        }))
        .when(!enabled, |button| button.opacity(0.4))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .focus(move |style| style.border_color(rgb(accent)))
                .on_click(cx.listener(move |view, _, window, cx| view.act(action, window, cx)))
        })
}

impl Review {
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        let selected = self.library.selected.as_ref();
        let row = selected.and_then(|key| {
            self.library
                .rows
                .iter()
                .position(|row| row.directory == key.directory)
        });
        let frame = selected.map_or(0, |key| key.frame);
        let last = self
            .detail
            .as_ref()
            .map_or(0, |detail| detail.frames.len().saturating_sub(1));
        let action = match event.keystroke.key.as_str() {
            "escape" => {
                window.remove_window();
                cx.stop_propagation();
                return;
            },
            "tab" => {
                if event.keystroke.modifiers.shift {
                    window.focus_prev();
                } else {
                    window.focus_next();
                }
                cx.stop_propagation();
                return;
            },
            "up" => Some(Action::Show(row.unwrap_or(0).saturating_sub(1))),
            "down" => Some(Action::Show(row.map_or(0, |row| row.saturating_add(1)))),
            "left" => Some(Action::Frame(frame.saturating_sub(1))),
            "right" => Some(Action::Frame(frame.saturating_add(1))),
            "home" => Some(Action::Frame(0)),
            "end" => Some(Action::Frame(last)),
            _ => None,
        };
        if let Some(action) = action {
            self.act(action, window, cx);
            cx.stop_propagation();
        }
    }
}
