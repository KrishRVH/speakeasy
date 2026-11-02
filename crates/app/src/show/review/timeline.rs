use std::time::Duration;

use gpui::{Bounds, Context, MouseButton, Pixels, canvas, div, prelude::*, px, relative, rgb};
use speakeasy_show::{Moment, Span};

use super::{Action, ImageState, Mode, Review, navigation::Stamp};
use crate::{
    controls::CORNER,
    gpui_ext::EntityUpdate as _,
    theme::{Palette, alpha},
};

#[derive(Default)]
pub(super) struct Timeline {
    pub(super) bounds: Bounds<Pixels>,
    dragging: bool,
}

impl Review {
    pub(super) fn timeline(&self, palette: &Palette, cx: &Context<'_, Self>) -> impl IntoElement {
        let covered = self.covered().unwrap_or_default();
        let (start, end) = (covered.start(), covered.end());
        let selected = self
            .library
            .selected
            .as_ref()
            .and_then(|key| self.detail.as_ref()?.frames.get(key.frame))
            .map_or(start, |frame| frame.at);
        let progress = fraction(selected, covered);
        let view = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .id("show-video-timeline")
                    .relative()
                    .w_full()
                    .h(px(24.0))
                    .rounded(CORNER)
                    .bg(rgb(palette.panel))
                    .border_1()
                    .border_color(palette.edge())
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .when(self.keyboard, |track| {
                        let lamp = palette.lamp;
                        track.focus(move |style| style.border_color(rgb(lamp)))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, event: &gpui::MouseDownEvent, _, cx| {
                            view.timeline.dragging = true;
                            view.scrub(event.position.x, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(|view, event: &gpui::MouseMoveEvent, _, cx| {
                        if !event.dragging() {
                            view.timeline.dragging = false;
                        } else if view.timeline.dragging {
                            view.scrub(event.position.x, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|view, _, _, _| view.timeline.dragging = false),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|view, _, _, _| view.timeline.dragging = false),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(relative(progress))
                            .bg(alpha(palette.lamp, 0.16)),
                    )
                    .children(
                        self.detail
                            .iter()
                            .flat_map(|detail| &detail.marks)
                            .filter(|&&(_, at)| covered.contains(at))
                            .map(|&(_, at)| {
                                div()
                                    .absolute()
                                    .left(relative(fraction(at, covered)))
                                    .top(px(3.0))
                                    .w(px(2.0))
                                    .h(px(16.0))
                                    .bg(rgb(palette.warn))
                            }),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(relative(progress))
                            .top_0()
                            .w(px(2.0))
                            .h_full()
                            .bg(rgb(palette.lamp)),
                    )
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                view.update_if_alive(cx, |view, _| view.timeline.bounds = bounds);
                            },
                            |_, (), _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .child(Stamp(start).to_string())
                    .child(Stamp(end).to_string()),
            )
    }

    fn covered(&self) -> Option<Span> {
        let movie = self.detail.as_ref()?.movie.as_ref()?;
        let coverage = movie.coverage.as_ref()?;
        Some(Span::new(
            Moment::from_millis(movie.origin_ms),
            Moment::from_millis(coverage.end_ms),
        ))
    }

    fn scrub(&mut self, x: Pixels, cx: &mut Context<'_, Self>) {
        if self.mode != Mode::Video || self.timeline.bounds.size.width <= px(0.0) {
            return;
        }
        let Some(covered) = self.covered() else {
            return;
        };
        let fraction = (x - self.timeline.bounds.left()) / self.timeline.bounds.size.width;
        let Ok(offset) = Duration::try_from_secs_f64(
            covered.end().since(covered.start()).as_secs_f64()
                * f64::from(fraction.clamp(0.0, 1.0)),
        ) else {
            return;
        };
        if let Some((index, _)) = self
            .detail
            .as_ref()
            .and_then(|detail| detail.locate(covered.start().saturating_add(offset)))
        {
            self.act(Action::Frame(index), cx);
        }
    }

    pub(super) fn video_caption(&self) -> String {
        match &self.image {
            ImageState::Ready(decoded) if self.loading.is_some() => {
                format!(
                    "Loading… · displayed sample at {} · {}",
                    Stamp(decoded.at.into()),
                    decoded.annotations.caption()
                )
            },
            ImageState::Ready(decoded) => format!(
                "Video sample at {} · {} ms · {}",
                Stamp(decoded.at.into()),
                decoded.at.as_millis(),
                decoded.annotations.caption()
            ),
            _ => "Seeking video frame…".into(),
        }
    }
}

fn fraction(at: Moment, span: Span) -> f32 {
    let length = span.end().since(span.start());
    if length.is_zero() {
        return 0.0;
    }
    at.since(span.start())
        .div_duration_f32(length)
        .clamp(0.0, 1.0)
}
