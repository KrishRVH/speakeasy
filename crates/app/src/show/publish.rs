use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Framing, NarrationLoss, Typeface, compose, select};

use super::{
    Checkpoint, Interrupted, SavedShow, recorder::NOTHING_RECORDED, store, store::checkpoint,
};
use crate::runtime::ports::{Footage, PUBLISH_STOPPED, Preservation, Published};

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

pub(crate) async fn publish(
    publication: Publication,
    permit: InsertPermit,
) -> anyhow::Result<Published> {
    let Publication {
        footage,
        checkpoint,
        origin,
    } = publication;

    let checkpoint = match checkpoint {
        Some(checkpoint) => match checkpoint.close().await {
            Ok(checkpoint) => checkpoint,
            Err(_cancelled) if !permit.active() => return Ok(Published::Cancelled),
            Err(error) => {
                return Err(error.context(
                    "The Show could not be saved because its checkpoint failed. Check Shows for recoverable footage.",
                ));
            },
        },
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
    let ending = footage.recording.ending;
    let lost = footage.recording.narration_lost;
    let completion = match lost {
        Some(NarrationLoss::AudioStopped | NarrationLoss::NotTranscribed) => {
            checkpoint::Publication::Pending
        },
        Some(NarrationLoss::Incomplete | NarrationLoss::AudioSilent) | None => {
            checkpoint::Publication::Narrated
        },
    };
    if completion == checkpoint::Publication::Narrated
        && let Some(checkpoint) = &checkpoint
    {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A failed checkpoint write keeps recovery audio; this bundle still carries the narration"
        )]
        let _ = checkpoint.narrate(
            &footage.recording.narration,
            lost == Some(NarrationLoss::Incomplete),
        );
    }
    if !permit.active() {
        return Ok(Published::Cancelled);
    }
    if matches!(origin, Origin::Recovered { pending: true })
        && completion == checkpoint::Publication::Pending
    {
        anyhow::bail!(
            "Narration could not be transcribed. The saved Show is unchanged; its audio remains available in Shows until its original expiry."
        );
    }
    let audio_kept = completion == checkpoint::Publication::Pending
        && checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.has_audio().unwrap_or(true));
    let result = persist(
        command,
        footage,
        checkpoint.as_ref(),
        completion,
        permit,
        parent,
    );
    match result {
        Ok(Some(Persisted {
            show,
            summary,
            source_lost,
            movie_lost,
        })) => {
            if !audio_kept {
                remove_published(checkpoint);
            }
            let preservation = Preservation {
                narration_lost: lost,
                source_lost,
                movie_lost,
                audio_kept,
            };
            match origin {
                Origin::Live => Ok(Published::Saved {
                    show,
                    ending,
                    preservation,
                    copied: copy(&summary),
                }),
                Origin::Recovered { pending } => Ok(Published::Recovered {
                    show,
                    preservation,
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
    let take = checkpoint.replay()?;
    let framing = take.framing();
    let (mut recording, journal) = take.finish();
    let mut speech = None;

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
        footage: Footage {
            recording,
            journal,
            framing,
        },
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
    movie_lost: bool,
}

fn persist(
    command: Option<&Path>,
    footage: Footage,
    checkpoint: Option<&Checkpoint>,
    completion: checkpoint::Publication,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Option<Persisted>> {
    let cancelled = || !permit.active();
    if cancelled() {
        return Ok(None);
    }
    let Footage {
        recording,
        journal,
        framing,
    } = footage;
    let duration = recording.duration;
    let voiced = recording.narration_lost != Some(NarrationLoss::AudioSilent);
    let selection = select(&recording, journal.samples()).context(NOTHING_RECORDED)?;
    let selection = match framing {
        Framing::Automatic => selection.framed(&recording, journal.samples(), journal.layouts()),
        Framing::Chosen => selection,
    };
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
    let mut staged = store::stage(
        &directory,
        bundle,
        Some(store::Retained {
            journal: &journal,
            duration,
            command,
        }),
    )?;
    let movie_lost = match checkpoint {
        Some(checkpoint) => checkpoint.retain_movie(&mut staged, duration)?,
        None => false,
    };
    if completion == checkpoint::Publication::Narrated
        && voiced
        && let Some(checkpoint) = checkpoint
    {
        checkpoint.retain_voice(&staged)?;
    }
    let source_lost = staged.source_lost;
    drop(journal);
    if !permit.commit() {
        return Ok(None);
    }
    if let Some(checkpoint) = checkpoint {
        checkpoint.commit(&staged, completion)?;
    }
    let (show, summary) = staged.persist()?;
    Ok(Some(Persisted {
        show,
        summary,
        source_lost,
        movie_lost,
    }))
}

#[cfg(test)]
mod tests;
