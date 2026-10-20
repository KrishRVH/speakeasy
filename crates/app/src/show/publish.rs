//! Publishing a finished Show: choosing and composing its keyframes, writing its bundle, and
//! copying its summary for the agent.

use std::{path::PathBuf, time::SystemTime};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Image, Journal, Narration, Recording, Typeface, compose, select};

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
        let Some(written) = write_bundle(footage, narration, &permit, store::private_directory)?
        else {
            return Ok(Published::Cancelled);
        };
        speakeasy_platform::copy_text(&written.summary).map_err(|error| {
            anyhow!("The Show was saved, but its summary could not be copied: {error}")
        })?;
        Ok(Published::Copied(written.directory))
    })
    .await
    // A failed join reports a fixed message, since a panic payload could carry private text.
    .unwrap_or_else(|_| Err(anyhow!(PUBLISH_STOPPED)))
}

/// A bundle in place, and the summary that names its files.
struct Written {
    directory: PathBuf,
    summary: String,
}

/// Writes a Show as a new bundle in the folder `parent` prepares, or nothing once `permit` is
/// revoked. `parent` runs only after the last cancellation check, so a cancelled Show never touches
/// the disk and never fails on a folder it did not need.
fn write_bundle(
    footage: Footage,
    narration: Narration,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Option<Written>> {
    let cancelled = || !permit.active();
    if cancelled() {
        return Ok(None);
    }
    let (recording, journal) = narrated(footage, narration);
    let selection = select(&recording, journal.samples())
        .context("The screen could not be recorded. Try again.")?;
    let frames = journal
        .frames_at(&selection.moments())
        .into_iter()
        .map(|frame| frame.context("A keyframe was not recorded"))
        .collect::<anyhow::Result<Vec<Image>>>()?;
    drop(journal);
    if cancelled() {
        return Ok(None);
    }
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    let bundle = compose(recording, selection, frames, &typeface)?;
    if cancelled() {
        return Ok(None);
    }
    let directory = store::fresh_directory(&parent()?, SystemTime::now())?;
    let staged = store::stage(&directory, &bundle.files(&directory)?)?;
    if !permit.commit() {
        return Ok(None);
    }
    staged.persist()?;
    Ok(Some(Written {
        summary: bundle.summary(&directory),
        directory,
    }))
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
