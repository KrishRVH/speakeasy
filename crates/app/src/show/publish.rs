//! Publishing a finished Show: choosing and composing its keyframes, writing its bundle, and
//! copying its summary for the agent.

use std::{path::Path, time::SystemTime};

use anyhow::Context as _;
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Image, Moment, Typeface, budget, compose, select};

use crate::ports::{Published, Recorded};

/// Composes and writes `recorded` off the caller's thread. The bundle appears and its summary is
/// copied only once `permit` commits; a publication cancelled before then stops at its next check
/// and deletes anything it staged.
pub(crate) async fn publish(recorded: Recorded, permit: InsertPermit) -> anyhow::Result<Published> {
    tokio::task::spawn_blocking(move || publish_now(recorded, &permit, &super::directory()))
        .await
        .context("Publishing the Show stopped unexpectedly. Try again.")?
}

/// Publishes `recorded` into a new directory under `parent`.
fn publish_now(
    recorded: Recorded,
    permit: &InsertPermit,
    parent: &Path,
) -> anyhow::Result<Published> {
    let cancelled = || !permit.active();
    if cancelled() {
        return Ok(Published::Cancelled);
    }
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
    if cancelled() {
        return Ok(Published::Cancelled);
    }
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    let bundle = compose(recording, selection, frames, &typeface)?;
    if cancelled() {
        return Ok(Published::Cancelled);
    }
    let directory = super::fresh_directory(parent, SystemTime::now())?;
    let staged = super::stage(&directory, &bundle.files(&directory)?)?;
    if !permit.commit() {
        return Ok(Published::Cancelled);
    }
    staged.persist()?;
    speakeasy_platform::copy_text(&bundle.summary(&directory))
        .context("The Show was saved, but its summary could not be copied")?;
    Ok(Published::Copied(directory))
}

#[cfg(test)]
mod tests {
    use speakeasy_platform::InputSender;
    use speakeasy_show::{Journal, Recording, Sample, Signature};

    use super::*;

    #[test]
    fn a_cancelled_show_never_reaches_the_disk() -> anyhow::Result<()> {
        let (sender, _inputs) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let permit = input.begin().context("a first permit")?;
        input.cancel();
        let frame = Image::from_bgra(2, 2, vec![128; 16])?;
        let mut journal = Journal::new();
        let samples = vec![Sample {
            at: Moment::START,
            signature: Signature::of(&frame),
        }];
        journal.record(Moment::START, frame);
        let recorded = Recorded {
            recording: Recording {
                duration: Moment::from_millis(1_000),
                ..Recording::default()
            },
            samples,
            journal,
        };
        let root = tempfile::tempdir()?;
        let parent = root.path().join("shows");
        let published = publish_now(recorded, &permit, &parent)?;
        assert!(matches!(published, Published::Cancelled));
        assert!(!parent.exists(), "Nothing was staged");
        Ok(())
    }
}
