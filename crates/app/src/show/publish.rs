use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Framing, Narration, NarrationLoss, Typeface, compose, select};

use super::{
    Checkpoint, Checkpointing, Interrupted, SavedShow, recorder::NOTHING_RECORDED, store,
    wsl::Lookup,
};
use crate::runtime::ports::{Footage, PUBLISH_STOPPED, Preservation, Published, Sealed};

const UNHEARD: &str = "Narration couldn't be transcribed. The saved Show is unchanged; its audio \
                       stays in Shows until it expires.";

pub(crate) struct Publication {
    pub footage: Footage,
    pub origin: Origin<Checkpointing>,
}

pub(crate) enum Origin<C> {
    Live { checkpoint: Option<C> },
    Recovered { checkpoint: C, pending: bool },
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
    let Some(origin) = settle(origin, &permit).await? else {
        return Ok(Published::Cancelled);
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
            store::directory,
            speakeasy_platform::copy_text,
        )
    })
    .await
    .unwrap_or_else(|_panicked| Err(anyhow!(PUBLISH_STOPPED)))
}

/// Waits for the checkpoint's files; `None` means the Show was cancelled, which excuses a failed
/// checkpoint.
async fn settle(
    origin: Origin<Checkpointing>,
    permit: &InsertPermit,
) -> anyhow::Result<Option<Origin<Checkpoint>>> {
    match origin.close().await {
        Ok(origin) => Ok(Some(origin)),
        Err(_) if !permit.active() => Ok(None),
        Err(error) => Err(error.context(
            "The Show couldn't be saved because its checkpoint failed. Check Shows for recoverable footage.",
        )),
    }
}

fn publish_with(
    handoff: Handoff<'_>,
    footage: Footage,
    origin: Origin<Checkpoint>,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
    copy: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Published> {
    if !permit.active() {
        return Ok(Published::Cancelled);
    }
    let ending = footage.recording.ending;
    let lost = footage.recording.narration_lost;
    let untranscribed = matches!(
        lost,
        Some(NarrationLoss::AudioStopped | NarrationLoss::NotTranscribed)
    );
    let (checkpoint, recovered) = match origin {
        Origin::Live { checkpoint } => (checkpoint, None),
        Origin::Recovered {
            checkpoint,
            pending,
        } => (Some(checkpoint), Some(pending)),
    };
    if recovered == Some(true) && (untranscribed || !heard(&footage.recording.narration)) {
        anyhow::bail!(UNHEARD);
    }
    let retry = untranscribed && checkpoint.as_ref().is_some_and(Checkpoint::has_audio);
    let Persisted {
        show,
        summary,
        source_lost,
        movie_lost,
    } = match persist(handoff, footage, checkpoint.as_ref(), retry, permit, parent) {
        Ok(Some(persisted)) => persisted,
        Ok(None) => return Ok(Published::Cancelled),
        Err(error) if checkpoint.is_some() => {
            return Err(error.context("The Show couldn't be saved. Recover it from Shows."));
        },
        Err(error) => return Err(error),
    };
    if !retry && let Some(checkpoint) = checkpoint {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The Show is saved; a checkpoint that cannot be removed stays in Shows until it is discarded or expires"
        )]
        let _ = checkpoint.discard();
    }
    let preservation = Preservation {
        narration_lost: lost,
        source_lost,
        movie_lost,
        audio_kept: retry,
    };
    Ok(match recovered {
        None => Published::Saved {
            show,
            ending,
            preservation,
            copied: copy(&summary),
        },
        Some(pending) => Published::Recovered {
            show,
            preservation,
            narrated_copy: pending,
        },
    })
}

pub(crate) async fn reopen(interrupted: Interrupted) -> anyhow::Result<Reopened> {
    tokio::task::spawn_blocking(move || reopen_with(&interrupted))
        .await
        .unwrap_or_else(|_panicked| {
            Err(anyhow!(
                "Recovery stopped. The interrupted Show is still available in Shows."
            ))
        })
}

fn reopen_with(interrupted: &Interrupted) -> anyhow::Result<Reopened> {
    let checkpoint = interrupted.lock()?;
    let mut footage = Footage::from(checkpoint.replay()?);
    let mut speech = None;
    match checkpoint.audio().ok().flatten() {
        Some(audio) => {
            if let Some(sealed) = crate::audio::seal_kept(&audio)? {
                speech = Some(RecoveredSpeech {
                    start: audio.start.saturating_add(sealed.trimmed),
                    sealed,
                    language: audio.language,
                    incomplete: !audio.complete,
                });
            }
        },
        None => footage.recording.narration_lost = Some(NarrationLoss::NotTranscribed),
    }
    Ok(Reopened {
        footage,
        pending: checkpoint.pending(),
        checkpoint,
        speech,
    })
}

fn heard(narration: &Narration) -> bool {
    match narration {
        Narration::Words(words) => words
            .iter()
            .any(|word| word.text.chars().any(char::is_alphanumeric)),
        Narration::Text(text) => text.chars().any(char::is_alphanumeric),
    }
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
    retry: bool,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Option<Persisted>> {
    let cancelled = || !permit.active();
    let Footage {
        recording,
        journal,
        framing,
    } = footage;
    let duration = recording.duration;
    let voiced = matches!(
        recording.narration_lost,
        None | Some(NarrationLoss::Incomplete)
    );
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
            journal.frame_at(at).context("A keyframe wasn't recorded")
        },
        &typeface,
    );
    if cancelled() {
        return Ok(None);
    }
    let bundle = bundle?;
    let recorded = checkpoint.map_or_else(SystemTime::now, Checkpoint::recorded);
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
    drop(journal);
    let movie_lost = match checkpoint {
        Some(checkpoint) => {
            if voiced {
                checkpoint.retain_voice(&staged)?;
            }
            checkpoint.retain_movie(&mut staged, duration)?
        },
        None => false,
    };
    let source_lost = staged.source_lost;
    if !permit.commit() {
        return Ok(None);
    }
    if retry && let Some(checkpoint) = checkpoint {
        checkpoint.keep_for_retry()?;
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
