//! Publishing a finished Show: choosing and composing its keyframes, writing its bundle, and
//! copying its summary for the agent.

use std::{path::PathBuf, time::SystemTime};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Journal, Narration, Recording, Typeface, compose, select};

use super::store;
use crate::runtime::ports::{Footage, PUBLISH_STOPPED, Published};

/// Composes and writes a Show from its `footage` and `narration` off the caller's thread. The
/// bundle appears and its summary is copied only once `permit` commits; a publication cancelled
/// before then stops at its next check and deletes anything it staged.
pub(crate) async fn publish(
    footage: Footage,
    narration: Narration,
    permit: InsertPermit,
) -> anyhow::Result<Published> {
    tokio::task::spawn_blocking(move || {
        let Some(summary) = write_bundle(footage, narration, &permit, store::private_directory)?
        else {
            return Ok(Published::Cancelled);
        };
        speakeasy_platform::copy_text(&summary).map_err(|error| {
            anyhow!("The Show was saved, but its summary could not be copied: {error}")
        })?;
        Ok(Published::Copied)
    })
    .await
    // A failed join reports a fixed message, since a panic payload could carry private text.
    .unwrap_or_else(|_| Err(anyhow!(PUBLISH_STOPPED)))
}

/// Writes a Show as a new bundle in the folder `parent` prepares and returns the summary that
/// names its files, or nothing once `permit` is revoked. A Show cancelled before or while it is
/// composed loads no further frames and never runs `parent`, one cancelled before its files are
/// staged never writes them, and one cancelled while they are staged stops at commit and deletes
/// them.
fn write_bundle(
    footage: Footage,
    narration: Narration,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Option<String>> {
    let cancelled = || !permit.active();
    if cancelled() {
        return Ok(None);
    }
    let (recording, journal) = narrated(footage, narration);
    let selection = select(&recording, journal.samples())
        .context("The screen could not be recorded. Try again.")?;
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    let bundle = compose(
        recording,
        selection,
        |at| {
            if cancelled() {
                return Err(anyhow!(PUBLISH_STOPPED));
            }
            journal.frame_at(at).context("A keyframe was not recorded")
        },
        &typeface,
    );
    // The bundle holds its encoded keyframes; the recorded frames, which can run to hundreds of
    // megabytes, are freed before it is written.
    drop(journal);
    if cancelled() {
        return Ok(None);
    }
    let bundle = bundle?;
    let directory = store::fresh_directory(&parent()?, SystemTime::now())?;
    let summary = bundle.summary(&directory);
    let files = bundle.into_files(&directory)?;
    if cancelled() {
        return Ok(None);
    }
    let staged = store::stage(&directory, &files)?;
    if !permit.commit() {
        return Ok(None);
    }
    staged.persist()?;
    Ok(Some(summary))
}

/// The Show as composition reads it, with `narration` already on its timeline, and the journal
/// its keyframes come from.
fn narrated(footage: Footage, narration: Narration) -> (Recording, Journal) {
    let Footage {
        surface,
        ended,
        journal,
        strokes,
        actions,
    } = footage;
    let recording = Recording {
        duration: ended,
        surface,
        strokes,
        actions,
        narration,
    };
    (recording, journal)
}

#[cfg(test)]
mod tests;
