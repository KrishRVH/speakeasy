use std::{sync::Arc, time::SystemTime};

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, ElementId, FontWeight, KeyDownEvent, MouseDownEvent,
    Pixels, SharedString, Stateful, Window, div, img, prelude::*, px, rgb,
};
use speakeasy_show::NarrationLoss;

use super::{
    Action, CopyKind, Direction, ExportKind, ImageState, Kind, Mode, Pane, Review, Source,
    library::{Detail, Key, Row},
    narration::{self, Narration},
    navigation::{Origin, Stamp},
};
use crate::{
    controls::{
        CONTROL_TEXT, DETAIL_LINE, DETAIL_TEXT, Kit, PANEL_CORNER, TITLE_TEXT, Tone, Weight,
    },
    icons,
    show::{
        Interrupted,
        store::{recall::Health, retention::Retention},
    },
    theme::{Palette, alpha},
};

const RETRY_NARRATION: &str = "Transcribe the kept audio with your configured local engine and the original \
    language. This saves a narrated copy; your existing Show and edits stay unchanged.";
const RECOVER_SHOW: &str = "Recover the frames, ink, marks, and audio that reached the checkpoint. Narration \
    uses your configured local engine and the original language.";
const RECOVERY_AUDIO: &str = "Recovery leaves your clipboard alone. Recovered voice stays private for video \
    export and expires seven days after the original recording, even if you keep the Show.";
const RECOVERY_PREFIX: &str = "A crash or a slow or full disk can leave a shorter recording. Recovery preserves \
    the original expiry date.";
const RECORD_HINT: &str =
    "Hold your shortcut and press S, W, or A to record your screen, a window, or an area.";
const KEYS_HINT: &str = "Page Up / Page Down: jump 10 seconds · Home / End: first / last frame";

pub(super) const NEARBY_FRAMES: usize = 10;
const SHOWN_FRAMES: usize = 2 * NEARBY_FRAMES + 1;

const BODY_LINE: Pixels = px(20.0);
const HEADING_TEXT: Pixels = px(16.0);

impl Render for Review {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let palette = self.theme.palette();
        let kit = Kit::new(palette).keyboard(self.keyboard);
        let now = SystemTime::now();
        let content = match self.library.selected.as_ref() {
            Some(key) => self.detail_pane(key, kit, palette, now, cx),
            None => self.placeholder(kit, palette).into_any_element(),
        };
        div()
            .id("shows")
            .size_full()
            .bg(rgb(palette.room))
            .text_color(rgb(palette.ink))
            .text_size(CONTROL_TEXT)
            .flex()
            .flex_col()
            .capture_key_down(cx.listener(|view, _: &KeyDownEvent, _, cx| {
                view.modality(true, cx);
            }))
            .capture_any_mouse_down(cx.listener(|view, _: &MouseDownEvent, _, cx| {
                view.menu = view.menu.pointer();
                view.modality(false, cx);
            }))
            .capture_key_up(cx.listener(Self::menu_key_up))
            .on_key_down(cx.listener(Self::key_down))
            .track_focus(&self.focus)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .child(self.library_pane(kit, palette, now, cx))
                    .child(content),
            )
            .when_some(self.notice.clone(), |body, notice| {
                body.child(
                    div()
                        .flex_none()
                        .px(px(20.0))
                        .py(px(12.0))
                        .border_t_1()
                        .border_color(palette.line())
                        .bg(rgb(palette.door))
                        .line_height(BODY_LINE)
                        .child(notice),
                )
            })
    }
}

impl Review {
    pub(super) fn modality(&mut self, keyboard: bool, cx: &mut Context<'_, Self>) {
        if self.keyboard != keyboard {
            self.keyboard = keyboard;
            cx.notify();
        }
    }

    fn library_pane(
        &self,
        kit: Kit<'_>,
        palette: &Palette,
        now: SystemTime,
        cx: &Context<'_, Self>,
    ) -> Div {
        div()
            .w(px(260.0))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(palette.line())
            .child(
                div()
                    .flex_none()
                    .h(px(56.0))
                    .pl(px(20.0))
                    .pr(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("shows-title")
                            .role(gpui::Role::Heading)
                            .aria_label("Shows")
                            .text_size(HEADING_TEXT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Shows"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(rgb(palette.muted))
                            .child(self.library.rows.len().to_string()),
                    )
                    .child(command(
                        kit,
                        Action::Refresh,
                        "Refresh",
                        Weight::Quiet,
                        self.listing.is_none() && self.keeping.is_none(),
                        cx,
                    )),
            )
            .when(self.source == Source::Demo, |pane| {
                pane.child(
                    kit.detail("Preview · synthetic recordings")
                        .px(px(20.0))
                        .pb(px(8.0)),
                )
            })
            .child(
                div()
                    .id("show-library")
                    .role(gpui::Role::ListBox)
                    .aria_label("Shows")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.library_scroll)
                    .px(px(8.0))
                    .pb(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .children(
                        self.library
                            .rows
                            .iter()
                            .enumerate()
                            .map(|(index, row)| self.show_row(index, row, kit, palette, now, cx)),
                    ),
            )
    }

    fn show_row(
        &self,
        index: usize,
        row: &Row,
        kit: Kit<'_>,
        palette: &Palette,
        now: SystemTime,
        cx: &Context<'_, Self>,
    ) -> Stateful<Div> {
        let current = self.library.is_selected(&row.directory);
        let recorded = row.recorded(now);
        let description = row.duration.map_or_else(
            || "Details unavailable".into(),
            |duration| format!("{duration} · {} frames", row.frames),
        );
        let tags: Vec<_> = (row.pinned == Some(true))
            .then_some(("Kept", Tone::Lamp))
            .into_iter()
            .chain(status(row))
            .collect();
        let label = [recorded.as_str(), &description]
            .into_iter()
            .chain(tags.iter().map(|&(tag, _)| tag))
            .collect::<Vec<_>>()
            .join(", ");
        let hover = palette.hover();
        pressable(
            kit,
            palette,
            Action::Show(self.library.offer(index)),
            label.into(),
            gpui::Role::ListBoxOption,
            true,
            cx,
        )
        .aria_selected(current)
        .relative()
        .w_full()
        .flex()
        .flex_col()
        .items_start()
        .gap(px(3.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(8.0))
        .when(current, |item| item.bg(hover).child(bar(palette)))
        .hover(move |style| style.bg(hover))
        .child(
            div()
                .text_size(TITLE_TEXT)
                .font_weight(FontWeight::MEDIUM)
                .child(recorded),
        )
        .child(kit.detail(description))
        .when(!tags.is_empty(), |item| {
            item.child(
                div()
                    .pt(px(4.0))
                    .flex()
                    .flex_wrap()
                    .gap(px(4.0))
                    .children(tags.into_iter().map(|(tag, tone)| kit.tag(tag, tone))),
            )
        })
    }

    fn placeholder(&self, kit: Kit<'_>, palette: &Palette) -> Div {
        let (title, text, keys) = if self.library.gone {
            (
                Some("Show gone"),
                "This Show was deleted or has expired. Choose another Show to continue.",
                false,
            )
        } else if self.listing.is_some() {
            (None, "Looking for Shows…", false)
        } else if !self.library.rows.is_empty() {
            (None, "Choose a Show to review.", false)
        } else if cfg!(target_os = "linux") {
            (
                Some("No Shows yet"),
                "Linux opens saved Shows but doesn't record them yet.",
                false,
            )
        } else {
            (Some("No Shows yet"), RECORD_HINT, true)
        };
        div()
            .flex_1()
            .p(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(380.0))
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .text_center()
                    .when_some(title, |column, title| {
                        column.child(
                            div()
                                .text_size(HEADING_TEXT)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                    })
                    .child(
                        div()
                            .line_height(BODY_LINE)
                            .text_color(rgb(palette.muted))
                            .child(text),
                    )
                    .when(keys, |column| {
                        column.child(
                            div()
                                .pt(px(8.0))
                                .flex()
                                .gap(px(6.0))
                                .children(["S", "W", "A"].map(|key| kit.keycap(key, false))),
                        )
                    }),
            )
    }

    fn detail_pane(
        &self,
        key: &Key,
        kit: Kit<'_>,
        palette: &Palette,
        now: SystemTime,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let row = self.library.selected_row();
        if let Some(row) = row
            && let Kind::Interrupted(interrupted) = &row.kind
        {
            return self
                .recovery(row, interrupted, kit, palette, now, cx)
                .into_any_element();
        }
        let title = row.map_or_else(|| "Show".into(), |row| row.recorded(now));
        let meta = row.map(|row| {
            let duration = row.duration.map_or_else(
                || "Duration unavailable".into(),
                |duration| duration.to_string(),
            );
            format!(
                "{duration} · {} keyframes · {} marks",
                row.frames, row.marks
            )
        });
        let tags = row.map_or_else(Vec::new, |row| {
            let kept = row.pinned == Some(true);
            std::iter::once(kit.tag(
                row.retention(now),
                if kept { Tone::Lamp } else { Tone::Muted },
            ))
            .chain(status(row).map(|(label, tone)| kit.tag(label, tone)))
            .collect()
        });
        div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .child(
                heading(kit, palette, title.into(), meta, tags)
                    .child(self.actions(row, kit, palette, now, cx)),
            )
            .when_some(self.confirm_delete, |body, offer| {
                let name = self.library.selected.as_ref()
                    .and_then(|key| key.directory.file_name())
                    .map_or_else(|| "this Show".into(), |name| name.to_string_lossy().into_owned());
                body.child(div().px(px(24.0)).pt(px(16.0)).child(confirm(Confirmation {
                    message: format!("Permanently delete {name}? This removes its images, video, summary, narration, and associated recovery audio, even if kept. Exported copies stay."),
                    cancel: Action::CancelDelete,
                    confirm: Action::ConfirmDelete(offer),
                    label: "Delete permanently",
                }, !self.can_delete(), kit, palette, cx)))
            })
            .child(self.review(key.frame, kit, palette, cx))
            .into_any_element()
    }

    fn actions(
        &self,
        row: Option<&Row>,
        kit: Kit<'_>,
        palette: &Palette,
        now: SystemTime,
        cx: &Context<'_, Self>,
    ) -> Div {
        let can_copy = self.can_copy(cx);
        let kept = row.and_then(|row| row.pinned);
        let keep = if self
            .keeping
            .as_ref()
            .is_some_and(|(directory, _)| self.library.is_selected(directory))
        {
            "Saving…"
        } else {
            keep_label(row, now)
        };
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .when(!cfg!(target_os = "linux"), |bar| {
                bar.child(command(
                    kit,
                    Action::Copy(CopyKind::Agent),
                    "Send to agent",
                    Weight::Primary,
                    can_copy,
                    cx,
                ))
                .child(command(
                    kit,
                    Action::Copy(CopyKind::Summary),
                    "Copy summary",
                    Weight::Secondary,
                    can_copy,
                    cx,
                ))
            })
            .when(self.export_cancel.is_some(), |bar| {
                bar.child(command(
                    kit,
                    Action::CancelExport,
                    "Cancel export",
                    Weight::Quiet,
                    true,
                    cx,
                ))
            })
            .child(command(
                kit,
                Action::Keep,
                keep,
                Weight::Quiet,
                kept.is_some()
                    && self.keeping.is_none()
                    && self.listing.is_none()
                    && self.deleting.is_none(),
                cx,
            ))
            .child(self.more(kit, palette, cx))
    }

    fn review(
        &self,
        frame: usize,
        kit: Kit<'_>,
        palette: &Palette,
        cx: &Context<'_, Self>,
    ) -> Stateful<Div> {
        let caption = self.request.and_then(|request| {
            let frame = self.detail.as_ref()?.frames.get(frame)?;
            match &self.image {
                ImageState::Ready(decoded) => Some(request.caption(frame, self.mode, decoded.at)),
                _ => None,
            }
        });
        div()
            .id("show-detail")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .px(px(24.0))
            .py(px(20.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            .when_some(
                self.detail
                    .as_ref()
                    .filter(|detail| !detail.warnings.is_empty()),
                |body, detail| body.child(warnings(detail, palette)),
            )
            .when_some(self.detail.as_ref(), |body, detail| {
                body.child(self.modes(detail, kit, cx))
            })
            .child(self.canvas(kit, palette))
            .when_some(caption, |body, caption| body.child(kit.detail(caption)))
            .child(self.navigation(frame, kit, palette, cx))
            .when(self.mode == Mode::Video, |body| {
                body.child(self.timeline(palette, cx))
            })
            .when(matches!(self.mode, Mode::Source | Mode::Video), |body| {
                body.child(self.marks(kit, cx))
            })
            .child(kit.detail(KEYS_HINT))
            .child(
                div().flex().child(
                    kit.segmented("show-panes", "Show text")
                        .child(segment(
                            kit,
                            Action::Pane(Pane::Summary),
                            "Summary",
                            self.pane == Pane::Summary,
                            true,
                            cx,
                        ))
                        .child(segment(
                            kit,
                            Action::Pane(Pane::Narration),
                            "Narration",
                            self.pane == Pane::Narration,
                            true,
                            cx,
                        )),
                ),
            )
            .child(match self.pane {
                Pane::Summary => self.summary(palette).into_any_element(),
                Pane::Narration => self.narration(kit, palette, cx).into_any_element(),
            })
    }

    fn modes(&self, detail: &Detail, kit: Kit<'_>, cx: &Context<'_, Self>) -> Div {
        let mode = |mode: Mode, label: &'static str, enabled: bool| {
            segment(
                kit,
                Action::Mode(mode),
                label,
                self.mode == mode,
                enabled,
                cx,
            )
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        kit.segmented("show-frame-modes", "Frames to review")
                            .child(mode(Mode::Keyframes, "Keyframes", true))
                            .child(mode(
                                Mode::Source,
                                "All retained frames",
                                detail.source_available,
                            ))
                            .when(
                                cfg!(any(target_os = "windows", target_os = "macos")),
                                |bar| bar.child(mode(Mode::Video, "Video", detail.movie.is_some())),
                            ),
                    )
                    .when(self.mode == Mode::Video, |row| {
                        row.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(kit.switch(
                                    id(Action::Annotations),
                                    "Annotations",
                                    self.annotated,
                                    press(Action::Annotations, !detail.frames.is_empty(), cx),
                                ))
                                .child("Annotations"),
                        )
                    }),
            )
            .child(kit.detail(detail.coverage.clone()))
    }

    fn canvas(&self, kit: Kit<'_>, palette: &Palette) -> Div {
        const UNAVAILABLE: &str =
            "Frame navigation is unavailable. The summary may still be readable.";
        div()
            .flex_1()
            .min_h(px(200.0))
            .overflow_hidden()
            .rounded(PANEL_CORNER)
            .bg(rgb(palette.panel))
            .border_1()
            .border_color(palette.line())
            .flex()
            .items_center()
            .justify_center()
            .child(match &self.image {
                ImageState::Ready(decoded) => img(Arc::clone(&decoded.image))
                    .size_full()
                    .into_any_element(),
                ImageState::Loading => quiet(palette, "Loading frame…").into_any_element(),
                ImageState::Failed(error) => div()
                    .w(px(452.0))
                    .max_w_full()
                    .p(px(16.0))
                    .child(kit.banner(Tone::Warn, "Frame unavailable", error.clone(), None))
                    .into_any_element(),
                ImageState::Empty if self.opening.is_some() => {
                    quiet(palette, "Opening Show…").into_any_element()
                },
                ImageState::Empty => quiet(palette, UNAVAILABLE).into_any_element(),
            })
    }

    fn navigation(
        &self,
        index: usize,
        kit: Kit<'_>,
        palette: &Palette,
        cx: &Context<'_, Self>,
    ) -> Div {
        let frames = self
            .detail
            .as_ref()
            .map_or(&[][..], |detail| detail.frames.as_slice());
        let whole_screen = self
            .detail
            .as_ref()
            .is_some_and(|detail| detail.source_available);
        let count = frames.len();
        let caption = if self.mode == Mode::Video {
            self.video_caption()
        } else {
            frames.get(index).map_or_else(
                || "No saved frame available".into(),
                |frame| frame.caption(count, whole_screen),
            )
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .flex_none()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(div().flex_1().min_w(px(0.0)).child(caption))
                    .child(kit.icon_button(
                        id(Action::Previous),
                        icons::CHEVRON_LEFT,
                        "Previous frame",
                        press(Action::Previous, index > 0 && count > 0, cx),
                    ))
                    .child(kit.icon_button(
                        id(Action::Next),
                        icons::CHEVRON_RIGHT,
                        "Next frame",
                        press(Action::Next, index.saturating_add(1) < count, cx),
                    )),
            )
            .child(
                div()
                    .id("show-frame-numbers")
                    .role(gpui::Role::TabList)
                    .aria_label("Frames")
                    .flex()
                    .flex_none()
                    .gap(px(6.0))
                    .overflow_x_scroll()
                    .track_scroll(&self.frame_scroll)
                    .children(
                        frames
                            .iter()
                            .enumerate()
                            .skip(index.saturating_sub(NEARBY_FRAMES))
                            .take(SHOWN_FRAMES)
                            .map(|(position, frame)| {
                                chip(
                                    kit,
                                    palette,
                                    Action::Frame(position),
                                    format!("#{} {}", frame.number, frame.at),
                                    index == position,
                                    cx,
                                )
                            }),
                    ),
            )
    }

    fn marks(&self, kit: Kit<'_>, cx: &Context<'_, Self>) -> Stateful<Div> {
        let enabled = self
            .detail
            .as_ref()
            .is_some_and(|detail| !detail.frames.is_empty());
        div()
            .id("show-marks")
            .max_h(px(72.0))
            .overflow_y_scroll()
            .flex()
            .flex_wrap()
            .gap(px(6.0))
            .children(
                self.detail
                    .iter()
                    .flat_map(|detail| &detail.marks)
                    .map(|&(number, _)| {
                        command(
                            kit,
                            Action::Mark(number),
                            format!("Mark #{number}"),
                            Weight::Secondary,
                            enabled,
                            cx,
                        )
                    }),
            )
    }

    fn summary(&self, palette: &Palette) -> Stateful<Div> {
        const REST: &str = "The rest is in summary.md. Copy summary includes the whole file.";
        let text = match self.detail.as_ref().map(|detail| &detail.summary) {
            Some(Ok(summary)) => {
                let shown = narration::excerpt(summary, narration::MOST_TEXT);
                div()
                    .child(shown.to_owned())
                    .when(shown.len() < summary.len(), |text| {
                        text.child(div().mt(px(8.0)).text_color(rgb(palette.warn)).child(REST))
                    })
            },
            Some(Err(error)) => div().text_color(rgb(palette.warn)).child(error.clone()),
            None => div()
                .text_color(rgb(palette.muted))
                .child(if self.opening.is_some() {
                    "Loading summary…"
                } else {
                    "Summary unavailable. Try Refresh or Open folder."
                }),
        };
        well(palette, "show-summary", px(160.0)).child(text)
    }

    fn narration(&self, kit: Kit<'_>, palette: &Palette, cx: &Context<'_, Self>) -> Stateful<Div> {
        let narration = self.detail.as_ref().map(|detail| &detail.narration);
        let incomplete = narration.is_some_and(Narration::incomplete);
        let truncated = narration.is_some_and(Narration::truncated);
        let text = match narration {
            Some(Narration::Timed { clauses, .. }) => {
                div().flex().flex_col().gap(px(2.0)).children(
                    clauses
                        .iter()
                        .enumerate()
                        .map(|(index, clause)| self.clause(index, clause, kit, palette, cx)),
                )
            },
            Some(Narration::Untimed { text, .. }) => div()
                .child(
                    div()
                        .text_color(rgb(palette.muted))
                        .child("These words have no timings and cannot point to frames."),
                )
                .child(div().mt(px(8.0)).child(text.clone())),
            Some(Narration::Silent) => div().child("No speech was recorded."),
            Some(Narration::Lost(loss)) => div().text_color(rgb(palette.warn)).child(match loss {
                NarrationLoss::Incomplete => {
                    "Only part of the audio was recovered; it contains no recognized words."
                },
                NarrationLoss::AudioStopped => "The microphone stopped. Narration is missing.",
                NarrationLoss::AudioSilent => {
                    "The microphone sent only silence. Narration is missing."
                },
                NarrationLoss::NotTranscribed => {
                    "Speech could not be transcribed. Narration is missing."
                },
            }),
            Some(Narration::Unavailable) => div()
                .child("Recorded narration is unavailable because show.json could not be read."),
            None => div()
                .text_color(rgb(palette.muted))
                .child(if self.opening.is_some() {
                    "Loading narration…"
                } else {
                    "Narration unavailable. Try Refresh or Open folder."
                }),
        };
        well(palette, "show-narration", px(180.0))
            .child(
                div()
                    .mb(px(8.0))
                    .text_size(DETAIL_TEXT)
                    .line_height(DETAIL_LINE)
                    .text_color(rgb(palette.muted))
                    .child("Recorded words; summary edits don't appear here."),
            )
            .when(incomplete, |body| {
                body.child(
                    div()
                        .mb(px(8.0))
                        .text_color(rgb(palette.warn))
                        .child("Narration is incomplete."),
                )
            })
            .child(text)
            .when(truncated, |body| {
                body.child(
                    div()
                        .mt(px(8.0))
                        .text_color(rgb(palette.warn))
                        .child("The rest of the recorded words are in show.json."),
                )
            })
    }

    fn clause(
        &self,
        index: usize,
        clause: &speakeasy_show::TimedClause,
        kit: Kit<'_>,
        palette: &Palette,
        cx: &Context<'_, Self>,
    ) -> Stateful<Div> {
        let chosen = self
            .request
            .is_some_and(|request| request.origin == Origin::Clause(index));
        let enabled = self
            .detail
            .as_ref()
            .is_some_and(|detail| !detail.frames.is_empty());
        let stamp = Stamp(clause.at).to_string();
        let hover = palette.hover();
        pressable(
            kit,
            palette,
            Action::Clause(index),
            format!("{stamp} {}", clause.text).into(),
            gpui::Role::Button,
            enabled,
            cx,
        )
        .w_full()
        .flex()
        .items_start()
        .gap(px(12.0))
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .when(chosen, |item| {
            item.bg(hover).border_color(alpha(palette.lamp, 0.6))
        })
        .when(enabled, |item| item.hover(move |style| style.bg(hover)))
        .child(
            div()
                .flex_none()
                .text_color(rgb(palette.muted))
                .child(stamp),
        )
        .child(div().flex_1().min_w(px(0.0)).child(clause.text.clone()))
    }

    fn recovery(
        &self,
        row: &Row,
        interrupted: &Interrupted,
        kit: Kit<'_>,
        palette: &Palette,
        now: SystemTime,
        cx: &Context<'_, Self>,
    ) -> Div {
        let pending = interrupted.narration_pending();
        let busy = self.recovering.is_some()
            || self.discarding.is_some()
            || self.keeping.is_some()
            || self.deleting.is_some();
        let lead = if pending {
            RETRY_NARRATION
        } else {
            RECOVER_SHOW
        };
        div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .child(
                heading(
                    kit,
                    palette,
                    if pending {
                        "Narration pending"
                    } else {
                        "Interrupted Show"
                    }
                    .into(),
                    Some(format!("{} · {}", row.recorded(now), row.retention(now))),
                    Vec::new(),
                )
                .child(command(
                    kit,
                    Action::Recover,
                    if pending {
                        "Transcribe narration"
                    } else {
                        "Recover Show"
                    },
                    Weight::Primary,
                    !busy,
                    cx,
                )),
            )
            .child(
                div()
                    .id("show-recovery")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .px(px(24.0))
                    .py(px(20.0))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(16.0))
                    .child(kit.group(vec![
                            div()
                                .max_w(px(640.0))
                                .p(px(16.0))
                                .flex()
                                .flex_col()
                                .gap(px(8.0))
                                .child(div().line_height(BODY_LINE).child(lead))
                                .child(kit.detail(RECOVERY_AUDIO))
                                .child(kit.detail(RECOVERY_PREFIX))
                                .into_any_element(),
                        ]))
                    .child(command(
                        kit,
                        Action::Discard,
                        if pending { "Discard audio" } else { "Discard" },
                        Weight::Destroy,
                        !busy && !self.confirm_discard,
                        cx,
                    ))
                    .when(self.confirm_discard, |pane| {
                        pane.child(confirm(
                            Confirmation {
                                message: if pending {
                                    super::DISCARD_AUDIO
                                } else {
                                    super::DISCARD_SHOW
                                }
                                .into(),
                                cancel: Action::CancelDiscard,
                                confirm: Action::ConfirmDiscard,
                                label: "Discard permanently",
                            },
                            busy,
                            kit,
                            palette,
                            cx,
                        ))
                    }),
            )
    }
}

fn status(row: &Row) -> impl Iterator<Item = (&'static str, Tone)> {
    [
        match &row.kind {
            Kind::Interrupted(show) if show.narration_pending() => {
                Some(("Narration pending", Tone::Lamp))
            },
            Kind::Interrupted(_) => Some(("Interrupted", Tone::Warn)),
            Kind::Saved => None,
        },
        row.narration_lost.map(|lost| match lost {
            NarrationLoss::Incomplete => ("Narration incomplete", Tone::Muted),
            NarrationLoss::AudioStopped
            | NarrationLoss::AudioSilent
            | NarrationLoss::NotTranscribed => ("No narration", Tone::Muted),
        }),
        (row.early && matches!(row.kind, Kind::Saved)).then_some(("Ended early", Tone::Warn)),
        match row.health {
            Health::Ok => None,
            Health::Unsupported => Some(("Can't open", Tone::Warn)),
            Health::Damaged => Some(("Damaged", Tone::Warn)),
        },
    ]
    .into_iter()
    .flatten()
}

fn keep_label(row: Option<&Row>, now: SystemTime) -> &'static str {
    if row.and_then(|row| row.pinned) != Some(true) {
        return "Keep Show";
    }
    let overdue = row
        .and_then(|row| row.directory.file_name())
        .and_then(|name| name.to_str())
        .is_some_and(
            |name| matches!(Retention::of(name, false), Retention::Expires(at) if at <= now),
        );
    if overdue {
        "Allow cleanup"
    } else {
        "Allow expiry"
    }
}

fn heading(
    kit: Kit<'_>,
    palette: &Palette,
    title: SharedString,
    meta: Option<String>,
    tags: Vec<Div>,
) -> Div {
    div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_start()
        .justify_between()
        .gap(px(16.0))
        .px(px(24.0))
        .pt(px(18.0))
        .pb(px(16.0))
        .border_b_1()
        .border_color(palette.line())
        .child(
            div()
                .flex_1()
                .min_w(px(200.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .text_size(HEADING_TEXT)
                        .line_height(px(24.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .when_some(meta, |column, meta| column.child(kit.detail(meta)))
                .when(!tags.is_empty(), |column| {
                    column.child(
                        div()
                            .pt(px(4.0))
                            .flex()
                            .flex_wrap()
                            .gap(px(6.0))
                            .children(tags),
                    )
                }),
        )
}

fn warnings(detail: &Detail, palette: &Palette) -> Stateful<Div> {
    div()
        .id("show-warnings")
        .flex_none()
        .max_h(px(96.0))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .px(px(14.0))
        .py(px(10.0))
        .rounded(PANEL_CORNER)
        .border_1()
        .border_color(alpha(palette.warn, 0.3))
        .bg(alpha(palette.warn, 0.1))
        .text_size(DETAIL_TEXT)
        .line_height(DETAIL_LINE)
        .children(
            detail
                .warnings
                .iter()
                .map(|warning| div().child(warning.clone())),
        )
}

fn well(palette: &Palette, id: &'static str, height: Pixels) -> Stateful<Div> {
    div()
        .id(id)
        .h(height)
        .flex_none()
        .overflow_y_scroll()
        .px(px(14.0))
        .py(px(12.0))
        .rounded(PANEL_CORNER)
        .bg(rgb(palette.panel))
        .border_1()
        .border_color(palette.line())
        .line_height(BODY_LINE)
}

fn quiet(palette: &Palette, text: &'static str) -> Div {
    div()
        .max_w(px(420.0))
        .px(px(16.0))
        .text_center()
        .line_height(BODY_LINE)
        .text_color(rgb(palette.muted))
        .child(text)
}

fn bar(palette: &Palette) -> Div {
    div()
        .absolute()
        .left(px(-1.0))
        .top(px(10.0))
        .bottom(px(10.0))
        .w(px(2.0))
        .rounded(px(1.0))
        .bg(rgb(palette.lamp))
}

struct Confirmation {
    message: String,
    cancel: Action,
    confirm: Action,
    label: &'static str,
}

fn confirm(
    confirmation: Confirmation,
    busy: bool,
    kit: Kit<'_>,
    palette: &Palette,
    cx: &Context<'_, Review>,
) -> Div {
    div()
        .max_w(px(640.0))
        .flex()
        .flex_col()
        .gap(px(12.0))
        .p(px(16.0))
        .rounded(PANEL_CORNER)
        .border_1()
        .border_color(alpha(palette.warn, 0.3))
        .bg(alpha(palette.warn, 0.06))
        .child(div().flex().child(kit.tag("Cannot be undone", Tone::Warn)))
        .child(div().line_height(BODY_LINE).child(confirmation.message))
        .child(
            div()
                .flex()
                .gap(px(8.0))
                .child(command(
                    kit,
                    confirmation.cancel,
                    "Cancel",
                    Weight::Secondary,
                    !busy,
                    cx,
                ))
                .child(command(
                    kit,
                    confirmation.confirm,
                    confirmation.label,
                    Weight::ConfirmDestroy,
                    !busy,
                    cx,
                )),
        )
}

pub(super) fn id(action: Action) -> ElementId {
    match action {
        Action::Mark(number) => ("show-mark", number).into(),
        Action::Recover => "recover-show".into(),
        Action::Discard => "discard-show".into(),
        Action::ConfirmDiscard => "confirm-discard-show".into(),
        Action::CancelDiscard => "cancel-discard-show".into(),
        Action::Delete(_) => "delete-show".into(),
        Action::ConfirmDelete(_) => "confirm-delete-show".into(),
        Action::CancelDelete => "cancel-delete-show".into(),
        Action::Jump(Direction::Forward) => "forward-show".into(),
        Action::Jump(Direction::Back) => "rewind-show".into(),
        Action::Mode(Mode::Keyframes) => "show-keyframes".into(),
        Action::Mode(Mode::Source) => "show-source-frames".into(),
        Action::Mode(Mode::Video) => "show-video-frames".into(),
        Action::Annotations => "show-video-annotations".into(),
        Action::Keep => "keep-show".into(),
        Action::Previous => "previous-frame".into(),
        Action::Next => "next-frame".into(),
        Action::Refresh => "shows-refresh".into(),
        Action::Copy(CopyKind::Agent) => "shows-send-agent".into(),
        Action::Copy(CopyKind::Summary) => "shows-copy".into(),
        Action::Copy(CopyKind::Evidence) => "shows-copy-evidence".into(),
        Action::Pane(Pane::Summary) => "show-summary-tab".into(),
        Action::Pane(Pane::Narration) => "show-narration-tab".into(),
        Action::Clause(index) => ("show-clause", index).into(),
        Action::Folder => "shows-folder".into(),
        Action::Movie => "shows-movie".into(),
        Action::Export(ExportKind::Archive) => "shows-export".into(),
        Action::Export(ExportKind::Video) => "shows-export-video".into(),
        Action::CancelExport => "shows-cancel-export".into(),
        Action::Show(offer) => ("show", offer.index).into(),
        Action::Frame(index) => ("frame", index).into(),
    }
}

fn press(
    action: Action,
    enabled: bool,
    cx: &Context<'_, Review>,
) -> Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static> {
    enabled.then(|| cx.listener(move |view, _, _, cx| view.act(action, cx)))
}

fn pressable(
    kit: Kit<'_>,
    palette: &Palette,
    action: Action,
    label: SharedString,
    role: gpui::Role,
    enabled: bool,
    cx: &Context<'_, Review>,
) -> Stateful<Div> {
    kit.pressable(id(action), label, role, false, press(action, enabled, cx))
        .border_1()
        .border_color(alpha(palette.lamp, 0.0))
}

fn command(
    kit: Kit<'_>,
    action: Action,
    label: impl Into<SharedString>,
    weight: Weight,
    enabled: bool,
    cx: &Context<'_, Review>,
) -> Stateful<Div> {
    kit.button(id(action), label, weight, press(action, enabled, cx))
}

fn segment(
    kit: Kit<'_>,
    action: Action,
    label: &'static str,
    selected: bool,
    enabled: bool,
    cx: &Context<'_, Review>,
) -> Stateful<Div> {
    kit.segment(id(action), label, selected, press(action, enabled, cx))
}

fn chip(
    kit: Kit<'_>,
    palette: &Palette,
    action: Action,
    label: String,
    selected: bool,
    cx: &Context<'_, Review>,
) -> Stateful<Div> {
    kit.segment(id(action), label, selected, press(action, true, cx))
        .h(px(28.0))
        .px(px(10.0))
        .bg(rgb(palette.raise))
        .border_color(if selected {
            rgb(palette.lamp)
        } else {
            palette.edge()
        })
        .text_size(DETAIL_TEXT)
        .text_color(rgb(if selected { palette.ink } else { palette.muted }))
}

impl Review {
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.menu.is_open() && self.menu_key(event.keystroke.key.as_str(), cx) {
            cx.stop_propagation();
            return;
        }
        let row = self.library.position();
        let frame = self.library.selected.as_ref().map_or(0, |key| key.frame);
        let last = self
            .detail
            .as_ref()
            .map_or(0, |detail| detail.frames.len().saturating_sub(1));
        let action = match event.keystroke.key.as_str() {
            "escape" if self.confirm_delete.is_some() => Some(Action::CancelDelete),
            "escape" => {
                window.remove_window();
                cx.stop_propagation();
                return;
            },
            "tab" => {
                if event.keystroke.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                cx.stop_propagation();
                return;
            },
            "up" => Some(Action::Show(
                self.library.offer(row.unwrap_or(0).saturating_sub(1)),
            )),
            "down" => Some(Action::Show(
                self.library
                    .offer(row.map_or(0, |row| row.saturating_add(1))),
            )),
            "left" => Some(Action::Frame(frame.saturating_sub(1))),
            "right" => Some(Action::Frame(frame.saturating_add(1))),
            "pageup" => Some(Action::Jump(Direction::Back)),
            "pagedown" => Some(Action::Jump(Direction::Forward)),
            "home" => Some(Action::Frame(0)),
            "end" => Some(Action::Frame(last)),
            _ => None,
        };
        if let Some(action) = action {
            self.act(action, cx);
            cx.stop_propagation();
        }
    }
}
