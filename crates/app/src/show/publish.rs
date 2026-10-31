//! Publishing a finished Show: choosing and composing its keyframes, writing its bundle, and
//! copying its summary for the agent.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Journal, NarrationLoss, Recording, Typeface, compose, select};

use super::{Checkpoint, Interrupted, SavedShow, recorder::NOTHING_RECORDED, store};
use crate::runtime::ports::{Footage, PUBLISH_STOPPED, Published};

/// Everything that publication owns. Captures have released their producer lanes before this
/// waits for the checkpoint; recovery holds the same files directly.
pub(crate) struct Publication {
    pub footage: Footage,
    pub checkpoint: Option<super::Checkpointing>,
    pub origin: Origin,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum Origin {
    #[default]
    Live,
    Recovered {
        pending: bool,
    },
}

pub(crate) struct Reopened {
    pub footage: Footage,
    pub checkpoint: Checkpoint,
    pub speech: Option<RecoveredSpeech>,
    pub pending: bool,
}

pub(crate) struct RecoveredSpeech {
    pub sealed: crate::runtime::ports::Sealed,
    pub start: std::time::Duration,
    pub language: Box<str>,
    pub incomplete: bool,
}

/// Composes and writes off the caller's thread, after the single checkpoint writer has drained.
pub(crate) async fn publish(
    publication: Publication,
    permit: InsertPermit,
) -> anyhow::Result<Published> {
    let Publication {
        footage,
        checkpoint,
        origin,
    } = publication;
    // A checkpoint failure must not cost the live in-memory Show. Its producers already report
    // failure during capture; the bundle remains useful even when recovery storage is unavailable.
    let checkpoint = match checkpoint {
        Some(checkpoint) => checkpoint.close().await.ok(),
        None => None,
    };
    tokio::task::spawn_blocking(move || {
        publish_with(
            crate::paths::command().as_deref(),
            footage,
            checkpoint,
            origin,
            &permit,
            store::private_directory,
            speakeasy_platform::copy_text,
        )
    })
    .await
    .unwrap_or_else(|_private_panic| Err(anyhow!(PUBLISH_STOPPED)))
}

fn publish_with(
    command: Option<&Path>,
    footage: Footage,
    checkpoint: Option<Checkpoint>,
    origin: Origin,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
    copy: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Published> {
    let Footage { recording, journal } = footage;
    let ending = recording.ending;
    let lost = recording.narration_lost;
    let transcribed = !matches!(
        lost,
        Some(NarrationLoss::AudioStopped | NarrationLoss::NotTranscribed)
    );
    if transcribed && let Some(checkpoint) = &checkpoint {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A failed checkpoint write keeps recovery audio; this bundle still carries the narration"
        )]
        let _ = checkpoint.narrate(
            &recording.narration,
            lost == Some(NarrationLoss::Incomplete),
        );
    }
    if !permit.active() {
        return Ok(Published::Cancelled);
    }
    if matches!(origin, Origin::Recovered { pending: true }) && !transcribed {
        anyhow::bail!(
            "Narration could not be transcribed. The saved Show is unchanged; its audio remains available in Shows until its original expiry."
        );
    }
    let audio_kept = !transcribed
        && checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.has_audio().unwrap_or(true));
    let result = persist(
        command,
        recording,
        journal,
        checkpoint.as_ref(),
        permit,
        parent,
    );
    match result {
        Ok(Some(Persisted {
            show,
            summary,
            source_lost,
        })) => {
            if !audio_kept {
                remove_published(checkpoint);
            }
            match origin {
                Origin::Live => Ok(Published::Saved {
                    show,
                    ending,
                    lost,
                    source_lost,
                    audio_kept,
                    copied: copy(&summary),
                }),
                Origin::Recovered { pending } => Ok(Published::Recovered {
                    show,
                    source_lost,
                    lost,
                    audio_kept,
                    narrated_copy: pending,
                }),
            }
        },
        Ok(None) => Ok(Published::Cancelled),
        Err(error) if checkpoint.is_some() => {
            Err(error.context("The Show could not be saved. Recover it from Shows."))
        },
        Err(error) => Err(error),
    }
}

/// Reads private files and prepares speech without opening a device, loading an engine, or
/// touching the clipboard. The session supplies the existing local worker after this returns.
pub(crate) async fn reopen(interrupted: Interrupted) -> anyhow::Result<Reopened> {
    tokio::task::spawn_blocking(move || reopen_with(interrupted))
        .await
        .unwrap_or_else(|_private_panic| {
            Err(anyhow!(
                "Recovery stopped. The interrupted Show is still available in Shows."
            ))
        })
}

fn reopen_with(interrupted: Interrupted) -> anyhow::Result<Reopened> {
    let checkpoint = interrupted.lock()?;
    let pending = checkpoint.pending()?;
    let (mut recording, journal) = checkpoint.replay()?.finish();
    let mut speech = None;
    // A corrupt transcript never suppresses surviving audio. Neither parser exposes private
    // content in an error; unreadable audio still permits an initial frame-only recovery.
    match checkpoint.narration() {
        Ok(Some((narration, incomplete))) => {
            recording.narration = narration;
            recording.narration_lost = incomplete.then_some(NarrationLoss::Incomplete);
        },
        Ok(None) | Err(_) => match checkpoint.audio() {
            Ok(Some(audio)) => {
                let incomplete = !audio.complete;
                let (sealed, start, language) = crate::audio::seal_kept(audio)?;
                if let Some(sealed) = sealed {
                    speech = Some(RecoveredSpeech {
                        start: start.saturating_add(sealed.trimmed),
                        sealed,
                        language,
                        incomplete,
                    });
                } else if incomplete {
                    recording.narration_lost = Some(NarrationLoss::Incomplete);
                }
            },
            Ok(None) | Err(_) => recording.narration_lost = Some(NarrationLoss::NotTranscribed),
        },
    }
    Ok(Reopened {
        footage: Footage { recording, journal },
        checkpoint,
        speech,
        pending,
    })
}

fn remove_published(checkpoint: Option<Checkpoint>) {
    if let Some(checkpoint) = checkpoint {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Durable publication markers let expiry cleanup retry without offering duplicate recovery"
        )]
        let _ = checkpoint.discard();
    }
}

struct Persisted {
    show: SavedShow,
    summary: String,
    source_lost: bool,
}

fn persist(
    command: Option<&Path>,
    recording: Recording,
    journal: Journal,
    checkpoint: Option<&Checkpoint>,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Option<Persisted>> {
    let cancelled = || !permit.active();
    if cancelled() {
        return Ok(None);
    }
    let duration = recording.duration;
    let selection = select(&recording, journal.samples()).context(NOTHING_RECORDED)?;
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    let bundle = compose(
        recording,
        selection,
        |at| {
            anyhow::ensure!(!cancelled(), PUBLISH_STOPPED);
            journal.frame_at(at).context("A keyframe was not recorded")
        },
        &typeface,
    );
    if cancelled() {
        return Ok(None);
    }
    let bundle = bundle?;
    let recorded = checkpoint
        .map(Checkpoint::recorded)
        .transpose()?
        .unwrap_or_else(SystemTime::now);
    let directory = store::fresh_directory(&parent()?, recorded)?;
    if cancelled() {
        return Ok(None);
    }
    let staged = store::stage(
        &directory,
        bundle,
        Some(store::Retained {
            journal: &journal,
            duration,
            command,
        }),
    )?;
    let source_lost = staged.source_lost;
    drop(journal);
    if !permit.commit() {
        return Ok(None);
    }
    if let Some(checkpoint) = checkpoint {
        checkpoint.commit(&staged)?;
    }
    let (show, summary) = staged.persist()?;
    Ok(Some(Persisted {
        show,
        summary,
        source_lost,
    }))
}

#[cfg(test)]
mod tests;
