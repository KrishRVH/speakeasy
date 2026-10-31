//! A time-scaled video scrubber requests indexed samples; the preview owner coalesces pending seeks.

use gpui::{
    Bounds, Context, MouseButton, Pixels, Window, canvas, div, prelude::*, px, relative, rgb,
};
use speakeasy_show::Moment;

use super::{Action, ImageState, Mode, Review, navigation::Stamp};
use crate::{
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
        let range = self
            .detail
            .as_ref()
            .and_then(|detail| detail.movie.as_ref())
            .and_then(|movie| {
                movie
                    .coverage
                    .as_ref()
                    .map(|coverage| [movie.origin_ms, coverage.end_ms])
            })
            .unwrap_or([0, 0]);
        let selected = self
            .library
            .selected
            .as_ref()
            .and_then(|key| self.detail.as_ref()?.frames.get(key.frame))
            .map_or(range[0], |frame| frame.at.as_millis());
        let progress = fraction(selected, range);
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
                    .rounded(px(4.0))
                    .bg(rgb(palette.room))
                    .border_1()
                    .border_color(alpha(palette.lamp, 0.3))
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, event: &gpui::MouseDownEvent, window, cx| {
                            view.timeline.dragging = true;
                            view.scrub(event.position.x, window, cx);
                        }),
                    )
                    .on_mouse_move(
                        cx.listener(|view, event: &gpui::MouseMoveEvent, window, cx| {
                            if !event.dragging() {
                                view.timeline.dragging = false;
                            } else if view.timeline.dragging {
                                view.scrub(event.position.x, window, cx);
                            }
                        }),
                    )
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
                            .bg(alpha(palette.lamp, 0.2)),
                    )
                    .children(
                        self.detail
                            .iter()
                            .flat_map(|detail| &detail.marks)
                            .filter(|(_, at)| (range[0]..=range[1]).contains(&at.as_millis()))
                            .map(|(_, at)| {
                                div()
                                    .absolute()
                                    .left(relative(fraction(at.as_millis(), range)))
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
                            .w(px(3.0))
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
                    .text_size(px(10.0))
                    .text_color(rgb(palette.muted))
                    .child(Stamp(Moment::from_millis(range[0])).to_string())
                    .child(Stamp(Moment::from_millis(range[1])).to_string()),
            )
    }

    fn scrub(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.mode != Mode::Video || self.timeline.bounds.size.width <= px(0.0) {
            return;
        }
        let fraction = (x - self.timeline.bounds.left()) / self.timeline.bounds.size.width;
        if !fraction.is_finite() {
            return;
        }
        let Some(detail) = &self.detail else {
            return;
        };
        let Some(movie) = &detail.movie else {
            return;
        };
        let Some(coverage) = &movie.coverage else {
            return;
        };
        let duration =
            std::time::Duration::from_millis(coverage.end_ms.saturating_sub(movie.origin_ms));
        let Ok(offset) = std::time::Duration::try_from_secs_f64(
            duration.as_secs_f64() * f64::from(fraction.clamp(0.0, 1.0)),
        ) else {
            return;
        };
        let at = Moment::from_millis(movie.origin_ms).saturating_add(offset);
        if let Some((index, _)) = detail.locate(at) {
            self.act(Action::Frame(index), window, cx);
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

fn fraction(at: u64, [start, end]: [u64; 2]) -> f32 {
    if end <= start {
        return 0.0;
    }
    (at.saturating_sub(start) as f32 / end.saturating_sub(start) as f32).clamp(0.0, 1.0)
}
