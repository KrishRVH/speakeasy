//! The Show library's native element tree. Private summary text is plain, inert text.

use std::time::SystemTime;

use gpui::{
    Context, Div, ElementId, FontWeight, KeyDownEvent, SharedString, Window, div, img, prelude::*,
    px, rgb,
};

use super::{
    Action, CopyKind, ImageState, Kind, Mode, Pane, Review, Source,
    narration::Narration,
    navigation::{Origin, Stamp},
};
use crate::{
    BRAND_FONT,
    show::store::{recall::Health, retention::Retention},
    theme::{Palette, alpha, mix},
};

const RETRY_NARRATION: &str = "Transcribe the kept audio with your configured local engine and the original \
    language. This saves a narrated copy; your existing Show and edits stay unchanged.";
const RECOVER_SHOW: &str = "Recover the frames, ink, marks, and audio that reached the checkpoint. Narration \
    uses your configured local engine and the original language.";
const RECOVERY_AUDIO: &str = "Recovery leaves your clipboard alone. Audio is deleted after successful \
    transcription and is never added to a saved Show bundle.";
const RECOVERY_PREFIX: &str = "A crash or a slow or full disk can leave a shorter recording. Recovery preserves \
    the original expiry date.";

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
                    .when_some(
                        match &row.kind {
                            Kind::Interrupted(show) => Some(show.narration_pending()),
                            Kind::Saved => None,
                        },
                        |row, pending| {
                            row.child(tag(
                                if pending {
                                    "Narration pending"
                                } else {
                                    "Interrupted"
                                },
                                palette.warn,
                            ))
                        },
                    )
                    .when_some(row.narration_lost, |row, lost| {
                        row.child(tag(
                            if lost == speakeasy_show::NarrationLoss::Incomplete {
                                "Narration incomplete"
                            } else {
                                "No narration"
                            },
                            palette.warn,
                        ))
                    })
                    .when(row.early && matches!(row.kind, Kind::Saved), |row| {
                        row.child(tag("Ended early", palette.warn))
                    })
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
                                    format!("{} Shows", self.library.rows.len())
                                },
                            )),
                    )
                    .child(
                        button(
                            Action::Refresh,
                            palette,
                            self.listing.is_none() && self.keeping.is_none(),
                            false,
                            cx,
                        )
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
        if let Some(Kind::Interrupted(interrupted)) = row.map(|row| &row.kind) {
            let pending = interrupted.narration_pending();
            let busy =
                self.recovering.is_some() || self.discarding.is_some() || self.keeping.is_some();
            return div()
                .flex_1()
                .p(px(24.0))
                .flex()
                .flex_col()
                .gap(px(16.0))
                .child(div().text_size(px(20.0)).child(if pending {
                    "Narration pending"
                } else {
                    "Interrupted Show"
                }))
                .child(row.map_or_else(String::new, |row| {
                    format!("{} · {}", row.recorded(now), row.retention(now))
                }))
                .child(if pending {
                    RETRY_NARRATION
                } else {
                    RECOVER_SHOW
                })
                .child(RECOVERY_AUDIO)
                .child(RECOVERY_PREFIX)
                .child(
                    div()
                        .flex()
                        .gap(px(10.0))
                        .child(button(Action::Recover, palette, !busy, false, cx).child(
                            if pending {
                                "Transcribe narration"
                            } else {
                                "Recover Show"
                            },
                        ))
                        .child(
                            button(
                                Action::Discard,
                                palette,
                                !busy && !self.confirm_discard,
                                false,
                                cx,
                            )
                            .child(if pending {
                                "Discard audio"
                            } else {
                                "Discard"
                            }),
                        ),
                )
                .when(self.confirm_discard, |pane| {
                    pane.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(if pending {
                                super::DISCARD_AUDIO
                            } else {
                                super::DISCARD_SHOW
                            })
                            .child(
                                div()
                                    .flex()
                                    .gap(px(10.0))
                                    .child(
                                        button(Action::CancelDiscard, palette, !busy, false, cx)
                                            .child("Cancel"),
                                    )
                                    .child(
                                        button(Action::ConfirmDiscard, palette, !busy, false, cx)
                                            .child("Discard permanently"),
                                    ),
                            ),
                    )
                })
                .into_any_element();
        }
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
            .when_some(self.detail.as_ref(), |body, detail| {
                body.child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(
                            button(
                                Action::Mode(Mode::Keyframes),
                                palette,
                                true,
                                self.mode == Mode::Keyframes,
                                cx,
                            )
                            .child("Keyframes"),
                        )
                        .child(
                            button(
                                Action::Mode(Mode::Source),
                                palette,
                                detail.source_available,
                                self.mode == Mode::Source,
                                cx,
                            )
                            .child("All retained frames"),
                        )
                        .when(cfg!(target_os = "windows"), |bar| {
                            bar.child(
                                button(
                                    Action::Mode(Mode::Video),
                                    palette,
                                    detail.movie.is_some(),
                                    self.mode == Mode::Video,
                                    cx,
                                )
                                .child("Video"),
                            )
                        }),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(palette.muted))
                        .child(detail.coverage.clone()),
                )
            })
            .child(self.canvas(palette))
            .when_some(
                self.request.and_then(|request| {
                    self.detail
                        .as_ref()
                        .and_then(|detail| detail.frames.get(key.frame))
                        .and_then(|frame| match &self.image {
                            ImageState::Ready(decoded) => {
                                Some(request.caption(frame, self.mode, decoded.at))
                            },
                            _ => None,
                        })
                }),
                |body, caption| {
                    body.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(palette.muted))
                            .child(caption),
                    )
                },
            )
            .child(self.navigation(key.frame, palette, cx))
            .when(self.mode == Mode::Video, |body| {
                body.child(self.timeline(palette, cx))
            })
            .when(matches!(self.mode, Mode::Source | Mode::Video), |body| {
                body.child(
                    div()
                        .id("show-marks")
                        .max_h(px(72.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.0))
                        .children(self.detail.iter().flat_map(|detail| &detail.marks).map(
                            |&(number, _)| {
                                button(
                                    Action::Mark(number),
                                    palette,
                                    self.detail
                                        .as_ref()
                                        .is_some_and(|detail| !detail.frames.is_empty()),
                                    false,
                                    cx,
                                )
                                .child(format!("Mark #{number}"))
                            },
                        )),
                )
            })
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(palette.muted))
                    .child("Page Up / Page Down: jump 10 seconds · Home / End: first / last frame"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        button(
                            Action::Pane(Pane::Summary),
                            palette,
                            true,
                            self.pane == Pane::Summary,
                            cx,
                        )
                        .child("Summary"),
                    )
                    .child(
                        button(
                            Action::Pane(Pane::Narration),
                            palette,
                            true,
                            self.pane == Pane::Narration,
                            cx,
                        )
                        .child("Narration"),
                    ),
            )
            .child(match self.pane {
                Pane::Summary => self.summary(palette).into_any_element(),
                Pane::Narration => self.narration(palette, cx).into_any_element(),
            })
            .child(self.commands(palette, now, cx))
            .into_any_element()
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
                ImageState::Ready(decoded) => img(std::sync::Arc::clone(&decoded.image))
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
        let caption = if self.mode == Mode::Video {
            self.video_caption()
        } else {
            frames.get(index).map_or_else(
                || "No saved frame available".into(),
                |frame| {
                    format!(
                        "#{} of {count} · {} · {} ms{}",
                        frame.number,
                        frame.at,
                        frame.at.as_millis(),
                        frame.marker
                    )
                },
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
                    .children(
                        frames
                            .iter()
                            .enumerate()
                            .skip(index.saturating_sub(10))
                            .take(21)
                            .map(|(position, frame)| {
                                button(
                                    Action::Frame(position),
                                    palette,
                                    true,
                                    index == position,
                                    cx,
                                )
                                .flex_none()
                                .child(format!("#{} {}", frame.number, frame.at))
                            }),
                    ),
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

    fn narration(&self, palette: &Palette, cx: &Context<'_, Self>) -> impl IntoElement {
        let narration = self.detail.as_ref().map(|detail| &detail.narration);
        let incomplete = matches!(
            narration,
            Some(
                Narration::Timed {
                    incomplete: true,
                    ..
                } | Narration::Untimed {
                    incomplete: true,
                    ..
                }
            )
        );
        let truncated = matches!(
            narration,
            Some(
                Narration::Timed {
                    truncated: true,
                    ..
                } | Narration::Untimed {
                    truncated: true,
                    ..
                }
            )
        );
        let text = match narration {
            Some(Narration::Timed { clauses, .. }) => div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .children(clauses.iter().enumerate().map(|(index, clause)| {
                    button(
                        Action::Clause(index),
                        palette,
                        self.detail
                            .as_ref()
                            .is_some_and(|detail| !detail.frames.is_empty()),
                        self.request
                            .is_some_and(|request| request.origin == Origin::Clause(index)),
                        cx,
                    )
                    .w_full()
                    .flex()
                    .items_start()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(palette.muted))
                            .child(Stamp(clause.at).to_string()),
                    )
                    .child(div().flex_1().min_w(px(0.0)).child(clause.text.clone()))
                })),
            Some(Narration::Untimed { text, .. }) => div()
                .child(
                    div()
                        .text_color(rgb(palette.muted))
                        .child("These words have no timings and cannot point to frames."),
                )
                .child(div().mt(px(8.0)).child(text.clone())),
            Some(Narration::Silent) => div().child("No speech was recorded."),
            Some(Narration::Lost(loss)) => div().text_color(rgb(palette.warn)).child(match loss {
                speakeasy_show::NarrationLoss::Incomplete => {
                    "Only part of the audio was recovered; it contains no recognized words."
                },
                speakeasy_show::NarrationLoss::AudioStopped => {
                    "The microphone stopped. Narration is missing."
                },
                speakeasy_show::NarrationLoss::NotTranscribed => {
                    "Speech could not be transcribed. Narration is missing."
                },
            }),
            Some(Narration::Unavailable) => div()
                .child("Recorded narration is unavailable because show.json could not be read."),
            None => div().child("Loading narration…"),
        };
        div()
            .id("show-narration")
            .h(px(180.0))
            .flex_none()
            .overflow_y_scroll()
            .p(px(12.0))
            .rounded(px(4.0))
            .bg(rgb(palette.room))
            .line_height(px(19.0))
            .child(
                div()
                    .mb(px(8.0))
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

    fn commands(
        &self,
        palette: &Palette,
        now: SystemTime,
        cx: &Context<'_, Self>,
    ) -> impl IntoElement {
        let can_copy =
            self.source != Source::Demo && (self.actions.can_copy)(cx) && self.copying.is_none();
        let row = self.library.selected.as_ref().and_then(|key| {
            self.library
                .rows
                .iter()
                .find(|row| row.directory == key.directory)
        });
        let kept = row.and_then(|row| row.pinned);
        let label = if kept == Some(true) {
            if row
                .and_then(|row| row.directory.file_name())
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    matches!(Retention::of(name, false), Retention::Expires(at) if at <= now)
                })
            {
                "Allow cleanup"
            } else {
                "Allow expiry"
            }
        } else {
            "Keep Show"
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .when(!cfg!(target_os = "linux"), |bar| {
                bar.child(
                    button(
                        Action::Copy(CopyKind::Summary),
                        palette,
                        can_copy,
                        false,
                        cx,
                    )
                    .child("Copy summary"),
                )
                .child(
                    button(
                        Action::Copy(CopyKind::Moment),
                        palette,
                        can_copy
                            && self.loading.is_none()
                            && matches!(self.image, ImageState::Ready(_)),
                        false,
                        cx,
                    )
                    .child("Copy moment"),
                )
            })
            .child(button(Action::Folder, palette, true, false, cx).child("Open folder"))
            .when(
                self.detail
                    .as_ref()
                    .is_some_and(|detail| detail.movie.is_some()),
                |bar| {
                    bar.child(button(Action::Movie, palette, true, false, cx).child("Play video"))
                },
            )
            .child(
                button(
                    Action::Export,
                    palette,
                    self.exporting.is_none() && self.detail.is_some(),
                    false,
                    cx,
                )
                .child(if self.exporting.is_some() {
                    "Exporting…"
                } else {
                    "Export ZIP…"
                }),
            )
            .child(
                button(
                    Action::Keep,
                    palette,
                    kept.is_some() && self.keeping.is_none() && self.listing.is_none(),
                    false,
                    cx,
                )
                .child(
                    if self
                        .keeping
                        .as_ref()
                        .is_some_and(|(directory, _)| self.library.is_selected(directory))
                    {
                        "Saving…"
                    } else {
                        label
                    },
                ),
            )
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
        Action::Mark(number) => SharedString::from(format!("show-mark-{number}")).into(),
        Action::Recover => "recover-show".into(),
        Action::Discard => "discard-show".into(),
        Action::ConfirmDiscard => "confirm-discard-show".into(),
        Action::CancelDiscard => "cancel-discard-show".into(),
        Action::Jump(true) => "forward-show".into(),
        Action::Jump(false) => "rewind-show".into(),
        Action::Mode(Mode::Keyframes) => "show-keyframes".into(),
        Action::Mode(Mode::Source) => "show-source-frames".into(),
        Action::Mode(Mode::Video) => "show-video-frames".into(),
        Action::Keep => "keep-show".into(),
        Action::Previous => "previous-frame".into(),
        Action::Next => "next-frame".into(),
        Action::Refresh => "shows-refresh".into(),
        Action::Copy(CopyKind::Summary) => "shows-copy".into(),
        Action::Copy(CopyKind::Moment) => "shows-copy-moment".into(),
        Action::Pane(Pane::Summary) => "show-summary-tab".into(),
        Action::Pane(Pane::Narration) => "show-narration-tab".into(),
        Action::Clause(index) => SharedString::from(format!("show-clause-{index}")).into(),
        Action::Folder => "shows-folder".into(),
        Action::Movie => "shows-movie".into(),
        Action::Export => "shows-export".into(),
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
            "pageup" => Some(Action::Jump(false)),
            "pagedown" => Some(Action::Jump(true)),
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
