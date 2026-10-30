//! The pill's element tree: the capsule, its slot or message, and the marks inside them. It reads
//! the pill's state and the scene of one frame and changes neither.

use gpui::{
    AnyElement, BoxShadow, Div, SharedString, canvas, div, point, prelude::*, px, rgb, rgba,
};
use speakeasy_core::motion::Spring;

use super::{
    Content, LID_CLOSED, Pill, RECORDING_WIDTH, SHADOW_BLUR, SHADOW_OFFSET, SHADOW_ROOM, Scene,
    grille::Grille, symbol::Symbol,
};
use crate::{
    runtime::Phase,
    theme::{Palette, alpha},
};

/// The slot row's horizontal padding.
const ROW_PADDING: f32 = 7.0;
/// The slot's horizontal inset inside the capsule: `ROW_PADDING` plus the capsule's one-pixel
/// border on each side.
const SLOT_SIDE_INSET: f32 = 2.0 * (ROW_PADDING + 1.0);
/// The slot's total vertical inset inside the capsule.
const SLOT_VERTICAL_INSET: f32 = 16.0;
/// The room the mark and clock leave before the row's padding at the capsule's rounded end, which
/// curves in closer to text than to the concentric slot.
const TRAILING_INSET: f32 = 7.0;
/// The lid position from which a closed slot's label fades in.
const LABEL_FADE_FROM: f32 = 0.6;

const TRAY_HINT: &str = if cfg!(target_os = "macos") {
    "In the menu bar · Use Speakeasy’s menu to quit"
} else {
    "In the tray · Use Speakeasy’s menu to quit"
};
const DRAW_HINT: &str = "Hold Shift to draw";

impl Pill {
    pub(super) fn render_capsule(&self, scene: Scene) -> Div {
        let palette = self.theme.palette();
        let height = self.capsule.height.value();
        let content = scene.content();
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
                    .when(content == Content::Slot, |capsule| {
                        capsule.child(brass_inlay(height, palette))
                    })
                    .child(self.render_content(content, scene, palette)),
            )
    }

    fn render_content(
        &self,
        content: Content,
        scene: Scene,
        palette: &'static Palette,
    ) -> AnyElement {
        match content {
            Content::TrayHint => div()
                .text_size(px(12.0))
                .child(TRAY_HINT)
                .into_any_element(),
            Content::Error => self
                .render_message(palette.warn, Some("Details in Settings"), palette)
                .into_any_element(),
            // Neither a success nor a warning: text that reached the clipboard rather than the
            // focused app, a Show copied short, one with nothing to record, or a message that
            // needs no action.
            Content::Notice => self
                .render_message(palette.muted, None, palette)
                .into_any_element(),
            Content::Slot => self.render_slot(scene, palette).into_any_element(),
            Content::Idle => idle_mark(palette).into_any_element(),
        }
    }

    /// The snapshot's message beside an attention mark in `color`, clipped to two lines above an
    /// optional `footer`.
    fn render_message(&self, color: u32, footer: Option<&'static str>, palette: &Palette) -> Div {
        div()
            .w_full()
            .px(px(14.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(Symbol::Attention.element(color))
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
        let slot_room = if shows_clock {
            width.min(RECORDING_WIDTH)
        } else {
            width
        };
        let slot_width = (slot_room - SLOT_SIDE_INSET).max(6.0);
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
                    .w(px(slot_width))
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
                                .top_0()
                                .left_0()
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
                    (scene.clock.to_string().into(), palette.warn)
                } else {
                    (scene.clock.to_string().into(), palette.muted)
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
        Phase::Starting => Some(marked(Symbol::Microphone, palette.muted, "Opening mic…")),
        Phase::Empty => Some(
            div()
                .text_size(px(12.0))
                .child("No speech detected")
                .into_any_element(),
        ),
        Phase::Copied if scene.labels_copy() => {
            Some(marked(Symbol::Check, palette.lamp, "Show copied"))
        },
        Phase::Done => Some(Symbol::Check.element(palette.lamp).into_any_element()),
        _ => None,
    }
}

/// `text` after `symbol`, which is drawn in `color`.
fn marked(symbol: Symbol, color: u32, text: &'static str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(7.0))
        .child(symbol.element(color))
        .child(div().text_size(px(12.0)).child(text))
        .into_any_element()
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
        .inset(px(2.5))
        .rounded(px(height / 2.0 - 3.5))
        .border_1()
        .border_color(alpha(palette.lamp, 0.16))
}
