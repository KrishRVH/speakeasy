use gpui::{
    AnyElement, BoxShadow, Div, SharedString, canvas, div, point, prelude::*, px, rgb, rgba,
};
use speakeasy_core::{machine::Phase, motion::Spring};
use speakeasy_platform::GlobeKey;

use super::{
    Content, LID_CLOSED, NOTE_HEIGHT, Note, Pill, RECORDING_WIDTH, SHADOW_BLUR, SHADOW_OFFSET,
    SHADOW_ROOM, Scene, grille::Grille,
};
use crate::{
    controls::{facet, hollow, icon},
    icons,
    theme::{Palette, alpha},
};

const ROW_PADDING: f32 = 7.0;
const SLOT_SIDE_INSET: f32 = 2.0 * (ROW_PADDING + 1.0);
const SLOT_VERTICAL_INSET: f32 = 16.0;
const TRAILING_INSET: f32 = 7.0;
const LABEL_FADE_FROM: f32 = 0.6;
const JEWEL: f32 = 14.0;

const TRAY_HINT: &str = if cfg!(target_os = "macos") {
    "Still running in the menu bar; quit from its menu."
} else {
    "Still running in the tray; quit from its menu."
};
const DRAW_HINT: &str = "Hold Shift to draw";
/// Keeps a note clear of the capsule's rounded ends.
const NOTE_INSET: f32 = 20.0;

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
                        inset: false,
                    }])
                    .overflow_hidden()
                    .opacity(self.capsule.opacity.value().clamp(0.0, 1.0))
                    .when(content.is_inlaid(), |capsule| {
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
            Content::Error => message_row(
                facet(palette.warn, JEWEL),
                self.snapshot.message.clone().into(),
                scene.footer(),
                palette,
            )
            .into_any_element(),
            Content::Notice => message_row(
                hollow(palette.lamp, JEWEL),
                self.snapshot.message.clone().into(),
                scene.footer(),
                palette,
            )
            .into_any_element(),
            Content::Slot => self.render_slot(scene, palette).into_any_element(),
            Content::Idle => idle_mark(palette).into_any_element(),
        }
    }

    fn render_slot(&self, scene: Scene, palette: &'static Palette) -> Div {
        let opened = self.capsule.note.value().clamp(0.0, 1.0);
        let height = self.capsule.height.value() - NOTE_HEIGHT * opened;
        // The capsule's 1 px border takes a pixel from each side of the row's room.
        let width = self.capsule.width.value().min(self.capsule.row.value()) - 2.0;
        let shows_clock = scene.shows_clock();
        let note = self
            .last_note
            .as_ref()
            .filter(|_| opened > 0.01)
            .map(|note| note_line(note, opened, palette));

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
        let row = div()
            .w(px(width))
            .h(px(height))
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

                let mark = match (scene.show, scene.hands_free) {
                    (true, _) => Some(icon(icons::SCREEN, 16.0, palette.live)),
                    (false, true) => Some(icon(icons::LOCK, 16.0, palette.muted)),
                    (false, false) => None,
                };

                row.child(
                    div()
                        .pr(px(TRAILING_INSET))
                        .flex()
                        .items_center()
                        .gap(px(11.0))
                        .children(mark)
                        .when_some(scene.label(), |row, label| {
                            row.child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(rgb(palette.muted))
                                    .child(label),
                            )
                        })
                        .child(div().text_size(px(11.0)).text_color(rgb(color)).child(text)),
                )
            });
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .child(row)
            .children(note)
    }
}

/// The note under the slot, clipped to its opening so it unrolls with the capsule.
fn note_line(note: &Note, opened: f32, palette: &Palette) -> Div {
    let text: SharedString = match note {
        Note::GlobeKey(GlobeKey::Emoji) => {
            "Fn also opens Emoji & Symbols; see Speakeasy Settings".into()
        },
        Note::GlobeKey(GlobeKey::InputSource) => {
            "Fn also switches the input source; see Speakeasy Settings".into()
        },
        Note::GlobeKey(GlobeKey::Dictation) => {
            "Double-tapping Fn also starts macOS Dictation; see Settings".into()
        },
        Note::SecureInput {
            holder: Some(holder),
        } => format!("Secure input blocks Space, Return, and Esc. {holder} turned it on.").into(),
        Note::SecureInput { holder: None } => "Secure input blocks Space, Return, and Esc.".into(),
        Note::DefaultMicrophone => "Your microphone isn't connected; using the default".into(),
    };
    div()
        .w_full()
        .h(px(NOTE_HEIGHT * opened))
        .px(px(NOTE_INSET))
        .overflow_hidden()
        .flex()
        .justify_center()
        .opacity(opened)
        .child(
            div()
                .flex_none()
                .max_w_full()
                .h(px(12.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(hollow(palette.lamp, 8.0))
                .child(
                    div()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(10.5))
                        .line_height(px(12.0))
                        .text_color(rgb(palette.muted))
                        .child(text),
                ),
        )
}

fn message_row(
    jewel: gpui::Svg,
    text: SharedString,
    footer: Option<&'static str>,
    palette: &Palette,
) -> Div {
    div()
        .w_full()
        .pl(px(19.0))
        .pr(px(14.0))
        .flex()
        .items_center()
        .gap(px(13.0))
        .child(jewel)
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .text_size(px(12.0))
                .line_height(px(16.0))
                .child(div().line_clamp(2).text_ellipsis().pr(px(6.0)).child(text))
                .when_some(footer, |message, footer| {
                    message.child(
                        div()
                            .mt(px(3.0))
                            .text_size(px(10.5))
                            .line_height(px(13.0))
                            .text_color(rgb(palette.muted))
                            .child(footer),
                    )
                }),
        )
}

fn slot_label(scene: &Scene, palette: &Palette) -> Option<AnyElement> {
    match scene.phase {
        Phase::Starting => Some(marked(icons::MICROPHONE, palette.muted, "Opening mic…")),
        Phase::Empty => Some(
            div()
                .text_size(px(12.0))
                .child("No speech detected")
                .into_any_element(),
        ),
        Phase::Copied if scene.labels_copy() => {
            Some(marked(icons::CHECK, palette.lamp, "Show copied"))
        },
        Phase::Done => Some(icon(icons::CHECK, 16.0, palette.lamp).into_any_element()),
        _ => None,
    }
}

fn marked(path: &'static str, color: u32, text: &'static str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(7.0))
        .child(icon(path, 16.0, color))
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
