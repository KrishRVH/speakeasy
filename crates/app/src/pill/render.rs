//! The pill's element tree: the capsule, its slot or message, and the marks inside them. It reads
//! the pill's state and the scene of one frame and changes neither.

use gpui::{
    AnyElement, BoxShadow, Div, SharedString, canvas, div, point, prelude::*, px, rgb, rgba,
};
use speakeasy_core::motion::Spring;

use super::{
    DRAW_HINT, LABEL_FADE_FROM, LID_CLOSED, Pill, RECORDING_WIDTH, ROW_PADDING, SHADOW_BLUR,
    SHADOW_OFFSET, SHADOW_ROOM, SLOT_SIDE_INSET, SLOT_VERTICAL_INSET, Scene, TRAILING_INSET,
    TRAY_HINT, grille::Grille, symbol::Symbol,
};
use crate::{
    runtime::Phase,
    theme::{Palette, alpha},
};

impl Pill {
    pub(super) fn render_capsule(&self, scene: Scene) -> Div {
        let palette = self.theme.palette();
        let height = self.capsule.height.value();
        let inlaid = scene.has_slot();
        div()
            .size_full()
            .flex()
            .items_end()
            .justify_center()
            .pb(px(SHADOW_ROOM - self.capsule.lift.value()))
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(self.capsule.width.value()))
                    .h(px(height))
                    .rounded(px(height / 2.0))
                    .bg(alpha(palette.door, 0.96))
                    .border_1()
                    .border_color(alpha(palette.ink, 0.13))
                    .text_color(rgb(palette.ink))
                    .shadow(vec![BoxShadow {
                        color: rgba(0x0000_0052).into(),
                        offset: point(px(0.0), px(SHADOW_OFFSET)),
                        blur_radius: px(SHADOW_BLUR),
                        spread_radius: px(0.0),
                    }])
                    .overflow_hidden()
                    .opacity(self.capsule.opacity.value().clamp(0.0, 1.0))
                    .when(inlaid, |capsule| {
                        capsule.child(brass_inlay(height, palette))
                    })
                    .child(self.render_content(scene, palette)),
            )
    }

    fn render_content(&self, scene: Scene, palette: &'static Palette) -> AnyElement {
        if scene.tray_hint.is_some() {
            div()
                .text_size(px(12.0))
                .child(TRAY_HINT)
                .into_any_element()
        } else if scene.phase == Phase::Error {
            self.render_message(
                Symbol::Attention,
                palette.warn,
                Some("Details in Settings"),
                palette,
            )
            .into_any_element()
        } else if scene.is_notice() {
            // Neither a success nor a warning: the text reached the clipboard, not the focused app.
            self.render_message(Symbol::Attention, palette.muted, None, palette)
                .into_any_element()
        } else if scene.has_slot() {
            self.render_slot(scene, palette).into_any_element()
        } else {
            idle_mark(palette).into_any_element()
        }
    }

    /// The snapshot's message beside `symbol`, clipped to two lines above an optional `footer`.
    fn render_message(
        &self,
        symbol: Symbol,
        color: u32,
        footer: Option<&'static str>,
        palette: &Palette,
    ) -> Div {
        div()
            .w_full()
            .px(px(14.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(symbol.element(color))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_size(px(12.0))
                    .line_height(px(16.0))
                    .child(
                        // GPUI truncates a clamped text as one long line, so its ellipsis can hang
                        // just past the wrap width; the padding keeps it inside the clip.
                        div()
                            .line_clamp(2)
                            .text_ellipsis()
                            .pr(px(6.0))
                            .child(self.snapshot.message.clone()),
                    )
                    .when_some(footer, |message, footer| {
                        message.child(div().text_color(rgb(palette.muted)).child(footer))
                    }),
            )
    }

    fn render_slot(&self, scene: Scene, palette: &'static Palette) -> Div {
        let (width, height) = (self.capsule.width.value(), self.capsule.height.value());
        let shows_clock = scene.shows_clock();
        // The clock, or the drawing hint in its place, takes any width beyond a recording capsule's.
        let slot_width = if shows_clock {
            width.min(RECORDING_WIDTH)
        } else {
            width
        } - SLOT_SIDE_INSET;
        let slot_height = (height - SLOT_VERTICAL_INSET).max(4.0);
        let grille = Grille {
            lid: self.capsule.lid.value().clamp(0.0, 1.0),
            waveform: self.capsule.waveform.value().clamp(0.0, 1.0),
            levels: self.meter.each_ref().map(Spring::value),
            reduced_motion: self.reduced_motion,
            sweep: scene.sweep(self.reduced_motion),
            palette,
        };
        div()
            .w_full()
            .px(px(ROW_PADDING))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(11.0))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(px(slot_width.max(6.0)))
                    .h(px(slot_height))
                    .rounded(px(slot_height / 2.0))
                    .bg(alpha(0x00_00_00, 0.5))
                    .border_1()
                    .border_color(if grille.shows_waveform() {
                        alpha(palette.live, 0.45 * grille.waveform)
                    } else {
                        alpha(palette.ink, 0.08)
                    })
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, (), window, cx| grille.paint(bounds, window, cx),
                        )
                        .size_full(),
                    )
                    .when_some(slot_label(&scene, palette), |slot, label| {
                        slot.child(
                            div()
                                .absolute()
                                .top(px(0.0))
                                .left(px(0.0))
                                .size_full()
                                .pl(px(10.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(
                                    ((grille.lid - LABEL_FADE_FROM)
                                        / (LID_CLOSED - LABEL_FADE_FROM))
                                        .clamp(0.0, 1.0),
                                )
                                .child(label),
                        )
                    }),
            )
            .when(shows_clock, |row| {
                let (text, color): (SharedString, _) = if scene.shows_draw_hint() {
                    (DRAW_HINT.into(), palette.ink)
                } else if scene.clock.near_limit {
                    (scene.clock.text().into(), palette.warn)
                } else {
                    (scene.clock.text().into(), palette.muted)
                };
                // A Show is always hands-free; its mark says the screen is recording too.
                let mark = match (scene.show, scene.hands_free) {
                    (true, _) => Some(Symbol::Screen.element(palette.live)),
                    (false, true) => Some(Symbol::Lock.element(palette.muted)),
                    (false, false) => None,
                };
                // Against the far end, whatever the text's width in this platform's font, so the
                // capsule's two ends keep matching margins.
                row.child(
                    div()
                        .pr(px(TRAILING_INSET))
                        .flex()
                        .items_center()
                        .gap(px(11.0))
                        .children(mark)
                        .child(div().text_size(px(11.0)).text_color(rgb(color)).child(text)),
                )
            })
    }
}

fn slot_label(scene: &Scene, palette: &Palette) -> Option<AnyElement> {
    match scene.phase {
        Phase::Starting => Some(
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(Symbol::Microphone.element(palette.muted))
                .child(div().text_size(px(12.0)).child("Opening mic…"))
                .into_any_element(),
        ),
        Phase::Empty => Some(
            div()
                .text_size(px(12.0))
                .child("No speech detected")
                .into_any_element(),
        ),
        Phase::Copied if scene.show => Some(
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(Symbol::Check.element(palette.lamp))
                .child(div().text_size(px(12.0)).child("Show copied"))
                .into_any_element(),
        ),
        Phase::Done => Some(Symbol::Check.element(palette.lamp).into_any_element()),
        _ => None,
    }
}

fn idle_mark(palette: &Palette) -> Div {
    div()
        .w(px(18.0))
        .h(px(2.0))
        .rounded_full()
        .bg(alpha(palette.ink, 0.5))
}

fn brass_inlay(height: f32, palette: &Palette) -> Div {
    div()
        .absolute()
        .top(px(2.5))
        .left(px(2.5))
        .right(px(2.5))
        .bottom(px(2.5))
        .rounded(px(height / 2.0 - 3.5))
        .border_1()
        .border_color(alpha(palette.lamp, 0.16))
}
