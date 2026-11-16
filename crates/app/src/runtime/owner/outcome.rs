use speakeasy_core::machine::{Failure, Notice, Outcome};
use speakeasy_show::{Ending, NarrationLoss};

use crate::runtime::{
    LOADING, SHOW_COPIED, SHOW_GONE,
    native::{Copied, Native, Restored},
    ports::{PUBLISH_STOPPED, Preservation, Unchosen},
};

const INSERT_FAILED: &str =
    "Cannot submit the text. Check desktop permissions and clipboard access, then try again.";

const REPLAY_HINT: &str = if cfg!(target_os = "macos") {
    " Your words are still available. Hold the dictation shortcut and press V, or choose Paste last dictation in the menu bar."
} else if cfg!(target_os = "windows") {
    " Your words are still available. Hold the dictation shortcut and press V, or choose Paste last dictation in the tray."
} else {
    " Your words are still available. Choose Paste last dictation in the tray, or run speakeasy paste-last."
};

pub(super) fn message(outcome: &Outcome<Native>, notice: Option<&Notice<Native>>) -> String {
    let mut message = match outcome {
        Outcome::NothingToPaste => "No dictation to paste yet.".into(),
        Outcome::Copied(message) => message.clone(),
        Outcome::Practiced => "Practice complete. Your words stay in Settings.".into(),
        Outcome::Failed(failed) => failure(failed),
        Outcome::ShowCopied(copied) => copied_message(*copied, notice),
        Outcome::Unchosen(Unchosen::Window) => {
            "No window to record under the pointer, so the Show was cancelled.".into()
        },
        Outcome::Unchosen(Unchosen::Area) => {
            "No area was chosen, so the Show was cancelled.".into()
        },
        Outcome::ShowGone => SHOW_GONE.into(),
        Outcome::Recovered(Restored {
            narrated_copy: true,
            ..
        }) => "Narrated copy saved. The original Show is unchanged.".into(),
        Outcome::Recovered(Restored { preservation, .. }) => match preservation.narration_lost {
            Some(
                NarrationLoss::NotTranscribed
                | NarrationLoss::AudioStopped
                | NarrationLoss::AudioSilent,
            ) => "Show recovered without narration. Find it in Shows.",
            Some(NarrationLoss::Incomplete) => {
                "Show recovered with incomplete narration. Find it in Shows."
            },
            None => "Show recovered. Find it in Shows.",
        }
        .into(),
        Outcome::Idle | Outcome::Done | Outcome::Empty | Outcome::Cancelled => {
            notice.map_or("", notice_text).into()
        },
    };
    if let Outcome::ShowCopied(Copied { preservation, .. })
    | Outcome::Recovered(Restored { preservation, .. }) = outcome
    {
        preserved(&mut message, *preservation);
    }
    message
}

pub(super) fn failure(failure: &Failure<Native>) -> String {
    match failure {
        Failure::Said(message) => message.clone(),
        Failure::InputLost => {
            "Speakeasy stopped watching the dictation shortcut. Resume dictation to try again."
                .into()
        },
        Failure::Undelivered { said, replay } => {
            let mut message = said.clone().unwrap_or_else(|| INSERT_FAILED.into());
            if *replay {
                message.push_str(REPLAY_HINT);
            }
            message
        },
        Failure::PublishStopped => PUBLISH_STOPPED.into(),
        Failure::NotCopied {
            saved: Copied {
                ending,
                preservation,
            },
            error,
            failure,
        } => {
            let show = if *ending == Ending::Finished {
                "The Show"
            } else {
                "The Show, which stopped early,"
            };
            let (narration, failure) = match preservation.narration_lost {
                Some(_) => (" without narration", failure.as_deref()),
                None => ("", None),
            };
            format!(
                "{show} was saved{narration} but not copied ({error}). Choose Copy last Show in \
                 Speakeasy's menu.{}",
                failure.map_or_else(String::new, |failure| format!(" {failure}"))
            )
        },
        Failure::RecoveryStorage(error) => format!("Show recovery storage failed: {error}"),
    }
}

fn copied_message(copied: Copied, notice: Option<&Notice<Native>>) -> String {
    let Copied {
        ending,
        preservation,
    } = copied;
    let copied = match (ending, preservation.narration_lost.is_some()) {
        (Ending::Finished, false) => SHOW_COPIED,
        (Ending::ScreenStopped, false) => "Show copied, but the screen stopped recording early.",
        (Ending::WindowClosed, false) => "Show copied, but the recorded window closed.",
        (Ending::WindowOffScreen, false) => "Show copied, but the recorded window went off screen.",
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
}

fn preserved(message: &mut String, preservation: Preservation) {
    if preservation.audio_kept {
        message.push_str(
            " Audio is kept privately for retry in Shows until its original seven-day expiry.",
        );
    }
    if preservation.movie_lost {
        message.push_str(" Video could not be saved; the selected keyframes are available.");
    }
    if preservation.source_lost {
        message.push_str(
            " Retained source frames could not be saved; the selected keyframes are available.",
        );
    }
}

fn notice_text(notice: &Notice<Native>) -> &str {
    match notice {
        Notice::CheckpointLost => {
            "Recovery stopped for this Show. Recording continues; finish it to save the full Show."
        },
        Notice::MovieStopped => {
            "Video stopped for this Show. Still frames and narration continue; finish to save what was recorded."
        },
        Notice::LoadingModel => LOADING,
        Notice::SettingsApplied => "Settings applied",
        Notice::WarmupFailed(failure) | Notice::NarrationLost(failure) => failure,
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
            let preservation = Preservation {
                source_lost,
                audio_kept,
                ..Preservation::default()
            };
            for (outcome, prefix) in [
                (
                    Outcome::ShowCopied(Copied {
                        ending: Ending::Finished,
                        preservation,
                    }),
                    SHOW_COPIED,
                ),
                (
                    Outcome::Recovered(Restored {
                        preservation,
                        narrated_copy: false,
                    }),
                    "Show recovered. Find it in Shows.",
                ),
            ] {
                assert_eq!(message(&outcome, None), format!("{prefix}{suffix}"));
            }
        }
    }
}
