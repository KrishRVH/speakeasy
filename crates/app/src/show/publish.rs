use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Framing, NarrationLoss, Typeface, compose, select};

use super::{
    Checkpoint, Checkpointing, Interrupted, SavedShow, recorder::NOTHING_RECORDED, store,
    store::checkpoint::Publication as Completion, wsl::Lookup,
};
use crate::runtime::ports::{Footage, PUBLISH_STOPPED, Preservation, Published, Sealed};

pub(crate) struct Publication {
    pub footage: Footage,
    pub origin: Origin<Checkpointing>,
}

pub(crate) enum Origin<C> {
    Live { checkpoint: Option<C> },
    Recovered { checkpoint: C, pending: bool },
}

impl<C> Origin<C> {
    const fn checkpoint(&self) -> Option<&C> {
        match self {
            Self::Live { checkpoint } => checkpoint.as_ref(),
            Self::Recovered { checkpoint, .. } => Some(checkpoint),
        }
    }
}

impl Origin<Checkpointing> {
    async fn close(self) -> anyhow::Result<Origin<Checkpoint>> {
        Ok(match self {
            Self::Live { checkpoint } => Origin::Live {
                checkpoint: match checkpoint {
                    Some(checkpoint) => checkpoint.close().await?,
                    None => None,
                },
            },
            Self::Recovered {
                checkpoint,
                pending,
            } => Origin::Recovered {
                checkpoint: checkpoint
                    .close()
                    .await?
                    .context("Show checkpoint stopped before returning its files")?,
                pending,
            },
        })
    }
}

#[derive(Clone, Copy, Default)]
struct Handoff<'a> {
    command: Option<&'a Path>,
    wsl: Option<&'a Lookup>,
}

pub(crate) struct Reopened {
    pub footage: Footage,
    pub checkpoint: Checkpoint,
    pub speech: Option<RecoveredSpeech>,
    pub pending: bool,
}

pub(crate) struct RecoveredSpeech {
    pub sealed: Sealed,
    pub start: Duration,
    pub language: Box<str>,
    pub incomplete: bool,
}

pub(crate) async fn publish(
    publication: Publication,
    permit: InsertPermit,
    wsl: Lookup,
) -> anyhow::Result<Published> {
    let Publication { footage, origin } = publication;
    let origin = match origin.close().await {
        Ok(origin) => origin,
        Err(_cancelled) if !permit.active() => return Ok(Published::Cancelled),
        Err(error) => {
            return Err(error.context(
                "The Show could not be saved because its checkpoint failed. Check Shows for recoverable footage.",
            ));
        },
    };
    tokio::task::spawn_blocking(move || {
        let command = crate::paths::command();
        publish_with(
            Handoff {
                command: command.as_deref(),
                wsl: Some(&wsl),
            },
            footage,
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
    handoff: Handoff<'_>,
    footage: Footage,
    origin: Origin<Checkpoint>,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
    copy: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Published> {
    let ending = footage.recording.ending;
    let lost = footage.recording.narration_lost;
    let completion = match lost {
        Some(NarrationLoss::AudioStopped | NarrationLoss::NotTranscribed) => Completion::Pending,
        Some(NarrationLoss::Incomplete | NarrationLoss::AudioSilent) | None => Completion::Narrated,
    };
    let checkpoint = origin.checkpoint();
    if completion == Completion::Narrated
        && let Some(checkpoint) = checkpoint
    {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A failed checkpoint write keeps recovery audio; this bundle still carries the narration"
        )]
        let _ = checkpoint.narrate(&footage.recording.narration, lost);
    }
    if !permit.active() {
        return Ok(Published::Cancelled);
    }
    if matches!(origin, Origin::Recovered { pending: true, .. })
        && completion == Completion::Pending
    {
        anyhow::bail!(
            "Narration could not be transcribed. The saved Show is unchanged; its audio remains available in Shows until its original expiry."
        );
    }
    let audio_kept = completion == Completion::Pending
        && checkpoint.is_some_and(|checkpoint| checkpoint.has_audio().unwrap_or(true));
    let result = persist(handoff, footage, checkpoint, completion, permit, parent);
    match result {
        Ok(Some(Persisted {
            show,
            summary,
            source_lost,
            movie_lost,
        })) => {
            let preservation = Preservation {
                narration_lost: lost,
                source_lost,
                movie_lost,
                audio_kept,
            };
            match origin {
                Origin::Live { checkpoint } => {
                    if !audio_kept && let Some(checkpoint) = checkpoint {
                        remove_published(checkpoint);
                    }
                    Ok(Published::Saved {
                        show,
                        ending,
                        preservation,
                        copied: copy(&summary),
                    })
                },
                Origin::Recovered {
                    checkpoint,
                    pending,
                } => {
                    if !audio_kept {
                        remove_published(checkpoint);
                    }
                    Ok(Published::Recovered {
                        show,
                        preservation,
                        narrated_copy: pending,
                    })
                },
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
    let mut footage = Footage::from(checkpoint.replay()?);
    let recording = &mut footage.recording;
    let mut speech = None;

    match checkpoint.narration() {
        Ok(Some((narration, loss))) => {
            recording.narration = narration;
            recording.narration_lost = loss;
        },
        Ok(None) | Err(_) => match checkpoint.audio() {
            Ok(Some(audio)) => {
                let incomplete = !audio.complete;
                if let Some(sealed) = crate::audio::seal_kept(&audio)? {
                    speech = Some(RecoveredSpeech {
                        start: audio.start.saturating_add(sealed.trimmed),
                        sealed,
                        language: audio.language,
                        incomplete,
                    });
                } else if pending {
                    recording.narration_lost = Some(if incomplete {
                        NarrationLoss::AudioStopped
                    } else {
                        NarrationLoss::NotTranscribed
                    });
                } else if incomplete {
                    recording.narration_lost = Some(NarrationLoss::Incomplete);
                }
            },
            Ok(None) | Err(_) => recording.narration_lost = Some(NarrationLoss::NotTranscribed),
        },
    }
    Ok(Reopened {
        footage,
        checkpoint,
        speech,
        pending,
    })
}

fn remove_published(checkpoint: Checkpoint) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Durable publication markers let expiry cleanup retry without offering duplicate recovery"
    )]
    let _ = checkpoint.discard();
}

struct Persisted {
    show: SavedShow,
    summary: String,
    source_lost: bool,
    movie_lost: bool,
}

fn persist(
    handoff: Handoff<'_>,
    footage: Footage,
    checkpoint: Option<&Checkpoint>,
    completion: Completion,
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
    let wsl = handoff.wsl.and_then(Lookup::found);
    if cancelled() {
        return Ok(None);
    }
    let mut staged = store::stage(
        store::managed(&directory, wsl.as_ref())?,
        bundle,
        Some(store::Retained {
            journal: &journal,
            duration,
            command: handoff.command,
        }),
    )?;
    let movie_lost = match checkpoint {
        Some(checkpoint) => checkpoint.retain_movie(&mut staged, duration)?,
        None => false,
    };
    if completion == Completion::Narrated
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
