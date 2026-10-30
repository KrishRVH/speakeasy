//! Publishing a finished Show: choosing and composing its keyframes, writing its bundle, and
//! copying its summary for the agent.

use std::{path::PathBuf, time::SystemTime};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Narration, NarrationLoss, Typeface, compose, select};

use super::{recorder::NOTHING_RECORDED, store};
use crate::runtime::ports::{Footage, PUBLISH_STOPPED, Published};

/// Composes and writes a Show from its `footage` and `narration`, with why that narration was
/// `lost` if it was, off the caller's thread. The bundle appears and its summary is copied only
/// once `permit` commits; a publication cancelled before then stops at its next check and deletes
/// anything it staged.
pub(crate) async fn publish(
    footage: Footage,
    narration: Narration,
    lost: Option<NarrationLoss>,
    permit: InsertPermit,
) -> anyhow::Result<Published> {
    let ending = footage.recording.ending;
    tokio::task::spawn_blocking(move || {
        let Some(summary) =
            write_bundle(footage, narration, lost, &permit, store::private_directory)?
        else {
            return Ok(Published::Cancelled);
        };
        speakeasy_platform::copy_text(&summary)
            .context("The Show was saved, but its summary could not be copied")?;
        Ok(Published::Copied { ending, lost })
    })
    .await
    // A failed join reports a fixed message, since a panic payload could carry private text.
    .unwrap_or_else(|_| Err(anyhow!(PUBLISH_STOPPED)))
}

/// Writes a Show, with its narration and why that was `lost` if it was, as a new bundle in the
/// folder `parent` prepares and returns the summary that names its files, or nothing once `permit`
/// is revoked. A Show cancelled before or while it is composed loads no further frames and never
/// runs `parent`, one cancelled before its files are staged never writes them, and one cancelled
/// while they are staged stops at commit and deletes them.
fn write_bundle(
    footage: Footage,
    narration: Narration,
    lost: Option<NarrationLoss>,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Option<String>> {
    let cancelled = || !permit.active();
    if cancelled() {
        return Ok(None);
    }
    let Footage {
        mut recording,
        journal,
    } = footage;
    recording.narration = narration;
    recording.narration_lost = lost;
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
    // The bundle holds its encoded keyframes; the recorded frames, which can run to hundreds of
    // megabytes, are freed before it is written.
    drop(journal);
    if cancelled() {
        return Ok(None);
    }
    let bundle = bundle?;
    let directory = store::fresh_directory(&parent()?, SystemTime::now())?;
    if cancelled() {
        return Ok(None);
    }
    let staged = store::stage(&directory, bundle)?;
    if !permit.commit() {
        return Ok(None);
    }
    staged.persist().map(Some)
}

#[cfg(test)]
mod tests;
