use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Framing, Narration, NarrationLoss, Typeface, compose, select};

use super::{
    Checkpoint, Checkpointing, Interrupted, SavedShow,
    recorder::NOTHING_RECORDED,
    store,
    store::checkpoint::{Damaged, Publication as Completion, SavedNarration},
    wsl::Lookup,
};
use crate::runtime::ports::{Audio, Footage, PUBLISH_STOPPED, Preservation, Published, Sealed};

const NO_SPEECH: &str = "Narration could not be recovered because the kept audio holds no speech. \
                         The saved Show is unchanged; its narration retry is removed from Shows.";
const UNREADABLE_NARRATION: &str = "Cannot read this Show's saved narration. Check access to its \
                                    folder; the interrupted Show stays in Shows";
const UNREADABLE_AUDIO: &str = "Cannot read this Show's kept audio. Check access to its folder; the \
                                interrupted Show stays in Shows";
const DAMAGED_NARRATION: &str = "This Show's saved narration is damaged and its kept audio holds no \
                                 speech, so its narration cannot be retried. The saved Show is \
                                 unchanged; discard this retry in Shows.";

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
            store::directory,
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
    let unheard = matches!(origin, Origin::Recovered { pending: true, .. })
        && (completion == Completion::Pending || !heard(&footage.recording.narration));
    if completion == Completion::Narrated
        && !unheard
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
    if unheard {
        anyhow::bail!(
            "Narration could not be transcribed. The saved Show is unchanged; its audio remains available in Shows until its original expiry."
        );
    }
    let audio_kept =
        completion == Completion::Pending && checkpoint.is_some_and(Checkpoint::keeps_retry);
    let audio = |voice_cut: bool| {
        if audio_kept {
            Audio::KeptForRetry
        } else if voice_cut && lost.is_none() {
            Audio::VoiceCutShort
        } else {
            Audio::Settled
        }
    };
    match persist(handoff, footage, checkpoint, completion, permit, parent) {
        Ok(Some(Persisted {
            show,
            summary,
            source_lost,
            movie_lost,
            voice_cut,
        })) => {
            let preservation = Preservation {
                narration_lost: lost,
                source_lost,
                movie_lost,
                audio: audio(voice_cut),
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

    let saved = match checkpoint.narration().context(UNREADABLE_NARRATION)? {
        SavedNarration::Saved(narration, _) if pending && !heard(&narration) => {
            SavedNarration::Absent
        },
        saved => saved,
    };
    let damaged = matches!(saved, SavedNarration::Damaged);
    match saved {
        SavedNarration::Saved(narration, loss) => {
            recording.narration = narration;
            recording.narration_lost = loss;
        },
        SavedNarration::Absent | SavedNarration::Damaged => match checkpoint.audio() {
            Ok(Some(audio)) => {
                let incomplete = !audio.complete;
                if let Some(sealed) = crate::audio::seal_kept(&audio)? {
                    speech = Some(RecoveredSpeech {
                        start: audio.start.saturating_add(sealed.trimmed),
                        sealed,
                        language: audio.language,
                        incomplete,
                    });
                } else if pending && damaged {
                    anyhow::bail!(DAMAGED_NARRATION);
                } else if pending {
                    checkpoint.discard()?;
                    anyhow::bail!(NO_SPEECH);
                } else if incomplete {
                    recording.narration_lost = Some(NarrationLoss::Incomplete);
                } else if crate::audio::silent_kept(&audio) {
                    recording.narration_lost = Some(NarrationLoss::AudioSilent);
                }
            },
            Err(error) if pending && !error.is::<Damaged>() => {
                return Err(error.context(UNREADABLE_AUDIO));
            },
            Ok(None) | Err(_) if pending && damaged => anyhow::bail!(DAMAGED_NARRATION),
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

fn heard(narration: &Narration) -> bool {
    match narration {
        Narration::Words(words) => words
            .iter()
            .any(|word| word.text.chars().any(char::is_alphanumeric)),
        Narration::Text(text) => text.chars().any(char::is_alphanumeric),
    }
}

fn remove_published(checkpoint: Checkpoint) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Durable publication markers let expiry cleanup retry without offering duplicate recovery"
    )]
    let _ = checkpoint.retire();
}

struct Persisted {
    show: SavedShow,
    summary: String,
    source_lost: bool,
    movie_lost: bool,
    voice_cut: bool,
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
    let voice_cut = match checkpoint {
        Some(checkpoint) if completion == Completion::Narrated && voiced => {
            checkpoint.retain_voice(&staged)?
        },
        Some(_) | None => false,
    };
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
        voice_cut,
    }))
}

#[cfg(test)]
mod tests;
