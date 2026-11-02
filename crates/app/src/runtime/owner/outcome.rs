use speakeasy_show::{Ending, NarrationLoss};

use crate::runtime::{
    LOADING, Phase, SHOW_COPIED, SHOW_GONE,
    ports::{Preservation, Unchosen},
};

pub(super) enum Outcome {
    Idle,
    Notice(&'static str),
    Done,
    Practiced,
    Copied(&'static str),
    ShowCopied {
        ending: Ending,
        preservation: Preservation,
    },
    ShowGone,
    Recovered {
        preservation: Preservation,
        narrated_copy: bool,
    },
    Empty,
    Cancelled,
    Unchosen(Unchosen),
    Failed(String),
}

impl Outcome {
    pub(super) fn message(&self, notice: Option<&Notice>) -> String {
        let mut message = match self {
            Self::Notice(message) | Self::Copied(message) => (*message).into(),
            Self::Practiced => "Practice complete. Your words stay in Settings.".into(),
            Self::Failed(message) => message.clone(),
            Self::ShowCopied {
                ending,
                preservation,
            } => {
                let copied = match (ending, preservation.narration_lost.is_some()) {
                    (Ending::Finished, false) => SHOW_COPIED,
                    (Ending::ScreenStopped, false) => {
                        "Show copied, but the screen stopped recording early."
                    },
                    (Ending::WindowClosed, false) => "Show copied, but the recorded window closed.",
                    (Ending::WindowOffScreen, false) => {
                        "Show copied, but the recorded window went off screen."
                    },
                    (Ending::Finished, true) => "Show copied without narration.",
                    (Ending::ScreenStopped, true) => {
                        "Show copied without narration, and the screen stopped recording early."
                    },
                    (Ending::WindowClosed, true) => {
                        "Show copied without narration, and the recorded window closed."
                    },
                    (Ending::WindowOffScreen, true) => {
                        "Show copied without narration, and the recorded window went off screen."
                    },
                };
                let mut message = copied.to_owned();
                if preservation.narration_lost.is_some()
                    && let Some(failure) = notice.and_then(Notice::failure)
                {
                    message.push(' ');
                    message.push_str(failure);
                }
                message
            },
            Self::Unchosen(Unchosen::Window) => {
                "No window to record under the pointer, so the Show was cancelled.".into()
            },
            Self::Unchosen(Unchosen::Area) => {
                "No area was chosen, so the Show was cancelled.".into()
            },
            Self::ShowGone => SHOW_GONE.into(),
            Self::Recovered {
                narrated_copy: true,
                ..
            } => "Narrated copy saved. The original Show is unchanged.".into(),
            Self::Recovered { preservation, .. } => match preservation.narration_lost {
                Some(NarrationLoss::NotTranscribed | NarrationLoss::AudioStopped) => {
                    "Show recovered without narration. Find it in Shows."
                },
                Some(NarrationLoss::Incomplete) => {
                    "Show recovered with incomplete narration. Find it in Shows."
                },
                None => "Show recovered. Find it in Shows.",
            }
            .into(),
            Self::Idle | Self::Done | Self::Empty | Self::Cancelled => {
                notice.map_or("", Notice::text).into()
            },
        };
        if let Self::ShowCopied { preservation, .. } | Self::Recovered { preservation, .. } = self {
            if preservation.audio_kept {
                message.push_str(
                    " Audio is kept privately for retry in Shows until its original seven-day expiry.",
                );
            }
            if preservation.movie_lost {
                message
                    .push_str(" Video could not be saved; the selected keyframes are available.");
            }
            if preservation.source_lost {
                message.push_str(
                    " Retained source frames could not be saved; the selected keyframes are available.",
                );
            }
        }
        message
    }

    pub(super) const fn phase(&self) -> Phase {
        match self {
            Self::Idle => Phase::Idle,
            Self::Done | Self::Practiced => Phase::Done,
            Self::Copied(_) | Self::ShowCopied { .. } => Phase::Copied,
            Self::Notice(_) | Self::ShowGone | Self::Recovered { .. } => Phase::Notice,
            Self::Empty => Phase::Empty,
            Self::Cancelled => Phase::Cancelled,
            Self::Unchosen(_) => Phase::Unchosen,
            Self::Failed(_) => Phase::Error,
        }
    }
}

pub(super) enum Notice {
    CheckpointLost,
    MovieStopped,
    LoadingModel,
    SettingsApplied,
    WarmupFailed(String),
    NarrationLost(String),
}

impl Notice {
    fn text(&self) -> &str {
        match self {
            Self::CheckpointLost => {
                "Recovery stopped for this Show. Recording continues; finish it to save the full Show."
            },
            Self::MovieStopped => {
                "Video stopped for this Show. Still frames and narration continue; finish to save what was recorded."
            },
            Self::LoadingModel => LOADING,
            Self::SettingsApplied => "Settings applied",
            Self::WarmupFailed(failure) | Self::NarrationLost(failure) => failure,
        }
    }

    pub(super) fn failure(&self) -> Option<&str> {
        match self {
            Self::WarmupFailed(failure) | Self::NarrationLost(failure) => Some(failure),
            Self::LoadingModel
            | Self::SettingsApplied
            | Self::CheckpointLost
            | Self::MovieStopped => None,
        }
    }

    pub(super) const fn is_warning(&self) -> bool {
        matches!(self, Self::CheckpointLost | Self::MovieStopped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_show_messages_report_retained_audio_and_lost_source_independently() {
        for (audio_kept, source_lost, suffix) in [
            (false, false, ""),
            (
                true,
                false,
                " Audio is kept privately for retry in Shows until its original seven-day expiry.",
            ),
            (
                false,
                true,
                " Retained source frames could not be saved; the selected keyframes are available.",
            ),
            (
                true,
                true,
                " Audio is kept privately for retry in Shows until its original seven-day expiry. \
                 Retained source frames could not be saved; the selected keyframes are available.",
            ),
        ] {
            for (outcome, prefix) in [
                (
                    Outcome::ShowCopied {
                        ending: Ending::Finished,
                        preservation: Preservation {
                            source_lost,
                            audio_kept,
                            ..Preservation::default()
                        },
                    },
                    SHOW_COPIED,
                ),
                (
                    Outcome::Recovered {
                        preservation: Preservation {
                            source_lost,
                            audio_kept,
                            ..Preservation::default()
                        },
                        narrated_copy: false,
                    },
                    "Show recovered. Find it in Shows.",
                ),
            ] {
                assert_eq!(outcome.message(None), format!("{prefix}{suffix}"));
            }
        }
    }
}
