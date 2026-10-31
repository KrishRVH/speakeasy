//! Real private checkpoint files and publication, with no microphone, speech engine, or clipboard.

use std::time::Duration;

use speakeasy_show::{Ending, Take, TakeEvent};

use super::*;

async fn checkpoint(parent: &Path) -> anyhow::Result<Checkpoint> {
    let surface = Size {
        width: 80.0,
        height: 60.0,
    };
    let (checkpoint, mut writer) =
        crate::show::Keeper::fixture(parent, SystemTime::now(), 1 << 20, surface)?;
    let mut take = Take::new(1 << 20, surface);
    assert!(
        writer.send(take.arrive(Moment::from_millis(100), || Some(desktop(
            80, 60, 0, 0, false
        )))?)
    );
    assert!(writer.send(TakeEvent::End {
        at: Moment::from_millis(500),
        ending: Ending::Finished
    }));
    assert!(writer.close().await);
    checkpoint.close().await
}

fn footage(checkpoint: &Checkpoint) -> anyhow::Result<Footage> {
    let (recording, journal) = checkpoint.replay()?.finish();
    Ok(Footage { recording, journal })
}

#[tokio::test]
async fn recovery_preserves_original_expiry_and_publishes_once_without_narration()
-> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    let checkpoint = checkpoint(parent.path()).await?;
    let recorded = checkpoint.recorded()?;
    drop(checkpoint);
    let interrupted = store::checkpoint::list(parent.path(), SystemTime::now())
        .shows
        .remove(0);
    let stale = interrupted.clone();
    let (_input, permit) = permit();
    let Published::Recovered { show, .. } =
        recover_with(interrupted, &permit, || Ok(parent.path().to_owned()))?
    else {
        bail!("Recovery did not publish");
    };
    let manifest = Manifest::parse(&fs::read(show.directory().join(Manifest::FILE))?)?;
    assert_eq!(
        manifest.recording().narration_lost,
        Some(NarrationLoss::NotTranscribed)
    );
    assert_eq!(manifest.recording().narration, Narration::default());
    assert_eq!(
        store::age::recorded_at(show.directory().file_name().unwrap().to_str().unwrap()),
        Some(recorded)
    );
    assert_eq!(store::source::inspect(show.directory())?.moments().len(), 1);
    assert!(stale.lock().is_err());
    assert_eq!(
        store::checkpoint::list(parent.path(), SystemTime::now()).shows,
        []
    );
    Ok(())
}

#[tokio::test]
async fn revoking_finished_publication_or_recovery_keeps_its_checkpoint() -> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    drop(checkpoint(parent.path()).await?);
    let interrupted = store::checkpoint::list(parent.path(), SystemTime::now())
        .shows
        .remove(0);
    let (input, revoked) = permit();
    input.cancel();
    assert!(matches!(
        recover_with(interrupted.clone(), &revoked, || bail!(
            "Cancelled recovery prepared storage"
        ))?,
        Published::Cancelled
    ));
    let (_input, active) = permit();
    assert!(
        recover_with(interrupted.clone(), &active, || bail!(
            "Synthetic disk failure"
        ))
        .is_err()
    );
    assert!(interrupted.lock()?.replay()?.ended());
    let checkpoint = interrupted.lock()?;
    let captured = footage(&checkpoint)?;
    assert!(matches!(
        publish_with(
            None,
            captured,
            Some(checkpoint),
            Origin::Live,
            &revoked,
            || bail!("Cancelled publication prepared storage"),
            |_| bail!("Cancelled publication copied")
        )?,
        Published::Cancelled
    ));
    assert!(interrupted.directory().exists());
    interrupted.lock()?.discard()?;
    assert!(!interrupted.directory().exists());
    Ok(())
}

#[tokio::test]
async fn failed_publication_keeps_recoverable_frames_and_success_cleans_up() -> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    let (_input, active) = permit();
    let checkpoint = checkpoint(parent.path()).await?;
    assert!(
        publish_with(
            None,
            footage(&checkpoint)?,
            Some(checkpoint),
            Origin::Live,
            &active,
            || bail!("Synthetic disk failure"),
            |_| bail!("Failed publication copied")
        )
        .is_err()
    );
    let interrupted = store::checkpoint::list(parent.path(), SystemTime::now())
        .shows
        .remove(0);
    let checkpoint = interrupted.lock()?;
    let captured = footage(&checkpoint)?;
    let (_input, active) = permit();
    assert!(matches!(
        publish_with(
            None,
            captured,
            Some(checkpoint),
            Origin::Live,
            &active,
            || Ok(parent.path().to_owned()),
            |_| Ok(())
        )?,
        Published::Saved { .. }
    ));
    assert!(!interrupted.directory().exists());
    assert_eq!(
        store::checkpoint::list(parent.path(), SystemTime::now()).shows,
        []
    );
    Ok(())
}

#[test]
fn optional_source_failure_still_saves_selected_keyframes_and_reports_the_loss()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (mut footage, narration) = rehearse(80, 60);
    // Delayed shutdown can exceed the sidecar format's timeline while selected keyframes stay valid.
    footage.recording.duration = Moment::from_millis(360_001);
    let (_input, permit) = permit();
    let result = publish_with(
        None,
        narrated(footage, narration, None),
        None,
        Origin::Live,
        &permit,
        || Ok(root.path().to_owned()),
        |_| Ok(()),
    )?;
    let Published::Saved {
        show,
        source_lost,
        copied: Ok(()),
        ..
    } = result
    else {
        bail!("Keyframes were not saved");
    };
    assert!(source_lost);
    assert!(store::recall::Entry::open(show.directory())?.show.is_ok());
    assert!(store::source::size(show.directory())?.is_none());
    assert!(fs::read_dir(show.directory())?.all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")
    }));
    Ok(())
}

fn recover_with(
    interrupted: Interrupted,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Published> {
    let reopened = reopen_with(interrupted)?;
    assert!(reopened.speech.is_none());
    publish_with(
        None,
        reopened.footage,
        Some(reopened.checkpoint),
        Origin::Recovered {
            pending: reopened.pending,
        },
        permit,
        parent,
        |_| anyhow::bail!("Recovery copied a summary"),
    )
}

mod audio;
mod movie;
