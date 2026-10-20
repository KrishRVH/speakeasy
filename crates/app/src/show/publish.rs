//! Publishing a finished Show: choosing and composing its keyframes, writing its bundle, and
//! copying its summary for the agent.

use std::time::SystemTime;

use anyhow::Context as _;
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Image, Moment, Typeface, budget, compose, select};

use crate::ports::{Published, Recorded};

/// Composes and writes `recorded` off the caller's thread. Nothing appears on disk or the
/// clipboard unless `permit` commits, so cancelling a Show while it publishes leaves no trace.
pub(crate) async fn publish(recorded: Recorded, permit: InsertPermit) -> anyhow::Result<Published> {
    tokio::task::spawn_blocking(move || publish_now(recorded, &permit))
        .await
        .context("Publishing the Show stopped unexpectedly. Try again.")?
}

fn publish_now(recorded: Recorded, permit: &InsertPermit) -> anyhow::Result<Published> {
    let Recorded {
        recording,
        samples,
        journal,
    } = recorded;
    let selection = select(
        &recording,
        &samples,
        budget(recording.duration.since(Moment::START)),
    );
    let frames = selection
        .keyframes()
        .iter()
        .map(|keyframe| {
            journal
                .frame_at(keyframe.at)
                .context("A keyframe was not recorded")
        })
        .collect::<anyhow::Result<Vec<Image>>>()?;
    drop(journal);
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    let bundle = compose(recording, selection, frames, &typeface)?;
    let directory = super::fresh_directory(&super::directory(), SystemTime::now())?;
    let staged = super::stage(&directory, &bundle.files(&directory)?)?;
    if !permit.commit() {
        return Ok(Published::Cancelled);
    }
    staged.persist()?;
    speakeasy_platform::copy_text(&bundle.summary(&directory))
        .context("The Show was saved, but its summary could not be copied")?;
    Ok(Published::Copied(directory))
}
