//! Publishing a finished Show: choosing and composing its keyframes, writing its bundle, and
//! copying its summary for the agent.

use std::{path::PathBuf, time::SystemTime};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Journal, Narration, NarrationLoss, Recording, Typeface, compose, select};

use super::{Checkpoint, Interrupted, SavedShow, recorder::NOTHING_RECORDED, store};
use crate::runtime::ports::{Footage, PUBLISH_STOPPED, Published};

/// Composes and writes a Show from its `footage` and `narration`, with why that narration was
/// `lost` if it was, off the caller's thread, then copies its summary. The bundle appears only once
/// `permit` commits; a publication cancelled before then stops at its next check and deletes
/// anything it staged.
pub(crate) async fn publish(
    footage: Footage,
    narration: Narration,
    lost: Option<NarrationLoss>,
    permit: InsertPermit,
) -> anyhow::Result<Published> {
    tokio::task::spawn_blocking(move || {
        publish_with(
            footage,
            narration,
            lost,
            &permit,
            store::private_directory,
            speakeasy_platform::copy_text,
        )
    })
    .await
    // A failed join reports a fixed message, since a panic payload could carry private text.
    .unwrap_or_else(|_| Err(anyhow!(PUBLISH_STOPPED)))
}

/// Writes a Show, with its narration and why that was `lost` if it was, as a new bundle in the
/// folder `parent` prepares, then hands the summary that names its files to `copy`. A Show
/// cancelled before or while it is composed loads no further frames and never runs `parent`, one
/// cancelled before its files are staged never writes them, and one cancelled while they are staged
/// stops at commit and deletes them. Once persisted, the Show stays saved whether or not `copy`
/// succeeds.
fn publish_with(
    footage: Footage,
    narration: Narration,
    lost: Option<NarrationLoss>,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
    copy: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Published> {
    let Footage {
        mut recording,
        journal,
        checkpoint,
    } = footage;
    let ending = recording.ending;
    recording.narration = narration;
    recording.narration_lost = lost;
    let result = persist(recording, journal, checkpoint.as_ref(), permit, parent);
    match result {
        Ok(Some(Persisted {
            show,
            summary,
            source_lost,
        })) => {
            remove_published(checkpoint);
            Ok(Published::Saved {
                show,
                ending,
                lost,
                source_lost,
                copied: copy(&summary),
            })
        },
        // Once capture finished, revocation is Pause, Quit, settings, or a desktop failure.
        // Unlock its checkpoint for recovery; only cancelling unfinished capture discards it.
        Ok(None) => Ok(Published::Cancelled),
        Err(error) if checkpoint.is_some() => {
            Err(error
                .context("The Show could not be saved. Recover its recorded frames from Shows."))
        },
        Err(error) => Err(error),
    }
}

/// Frame-only recovery enters the owner's publication stage. It never opens a microphone or a
/// speech worker and has no clipboard callback. Cancellation leaves the original checkpoint.
pub(crate) async fn recover(
    interrupted: Interrupted,
    permit: InsertPermit,
) -> anyhow::Result<Published> {
    tokio::task::spawn_blocking(move || {
        recover_with(interrupted, &permit, store::private_directory)
    })
    .await
    .unwrap_or_else(|_private_panic| {
        Err(anyhow!(
            "Recovery stopped. The interrupted Show is still available in Shows."
        ))
    })
}

fn recover_with(
    interrupted: Interrupted,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Published> {
    let checkpoint = interrupted.lock()?;
    let (mut recording, journal) = checkpoint.replay()?.finish();
    recording.narration_lost = Some(NarrationLoss::NotTranscribed);
    match persist(recording, journal, Some(&checkpoint), permit, parent)? {
        Some(Persisted {
            show, source_lost, ..
        }) => {
            remove_published(Some(checkpoint));
            Ok(Published::Recovered { show, source_lost })
        },
        None => Ok(Published::Cancelled),
    }
}

fn remove_published(checkpoint: Option<Checkpoint>) {
    if let Some(checkpoint) = checkpoint {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The published marker and bundle provenance are durable; expiry cleanup retries removal without offering duplicate recovery"
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
    let staged = store::stage(&directory, bundle)?;
    let source_lost = staged.write_source(&journal, duration).is_err();
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
