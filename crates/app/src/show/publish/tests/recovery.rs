use std::time::Duration;

use speakeasy_show::{Ending, Take, TakeEvent};

use super::*;

fn recoverable(parent: &Path) -> Vec<Interrupted> {
    store::checkpoint::list(parent, SystemTime::now())
}

fn interrupted(parent: &Path) -> anyhow::Result<Interrupted> {
    let Ok([show]) = <[_; 1]>::try_from(recoverable(parent)) else {
        bail!("Expected one interrupted Show");
    };
    Ok(show)
}

async fn checkpoint(parent: &Path) -> anyhow::Result<Checkpoint> {
    let surface = Size {
        width: 80.0,
        height: 60.0,
    };
    let (checkpoint, mut writer) =
        crate::show::Keeper::fixture(parent, 1 << 20, surface, Framing::Automatic)?;
    let mut take = Take::new(1 << 20, surface, Framing::Automatic);
    assert!(
        writer.send(take.arrive(Moment::from_millis(100), || Some(desktop(
            80, 60, 0, 0, false
        )))?)
    );
    assert!(writer.send(take.end(Moment::from_millis(500), Ending::Finished)));
    drop(writer);
    checkpoint
        .close()
        .await?
        .context("Fixture checkpoint is missing")
}

async fn clicked_checkpoint(parent: &Path, framing: Framing) -> anyhow::Result<Checkpoint> {
    let (width, height) = (1_600, 1_000);
    let surface = Size {
        width: width as f32,
        height: height as f32,
    };
    let budget = speakeasy_show::log::MAX_BUDGET;
    let (checkpoint, mut writer) = crate::show::Keeper::fixture(parent, budget, surface, framing)?;
    let mut take = Take::new(budget, surface, framing);
    let frame = |at: u64| Some(desktop(width, height, 0, 0, at > 1_000));
    assert!(writer.send(take.arrive(Moment::START, || frame(0))?));
    for (at, pressed) in [(1_000, true), (1_080, false)] {
        let click = TakeEvent::Pointer {
            part: Rect::from([0.0, 0.0, 1.0, 1.0]),
            at: Moment::from_millis(at),
            point: Point::new(0.15, 0.2),
            drawing: false,
            pressed,
        };
        take.apply(&click)?;
        assert!(writer.send(click));
    }
    assert!(writer.send(take.arrive(Moment::from_millis(1_200), || frame(1_200))?));
    assert!(writer.send(take.end(Moment::from_millis(2_000), Ending::Finished)));
    drop(writer);
    checkpoint
        .close()
        .await?
        .context("Fixture checkpoint is missing")
}

#[tokio::test]
async fn recovery_frames_a_show_as_its_recording_chose() -> anyhow::Result<()> {
    for framing in [Framing::Automatic, Framing::Chosen] {
        let parent = tempfile::tempdir()?;
        drop(clicked_checkpoint(parent.path(), framing).await?);
        let interrupted = interrupted(parent.path())?;
        let (_input, permit) = permit();
        let Published::Recovered { show, .. } =
            recover_with(interrupted, &permit, || Ok(parent.path().to_owned()))?
        else {
            bail!("Recovery did not publish");
        };
        let manifest = Manifest::parse(&fs::read(show.directory().join(Manifest::FILE))?)?;
        let cropped = manifest
            .keyframes()
            .iter()
            .any(|keyframe| keyframe.crop.is_some());
        assert_eq!(cropped, framing == Framing::Automatic, "{framing:?}");
    }
    Ok(())
}

#[tokio::test]
async fn recovery_preserves_original_expiry_and_publishes_once_without_narration()
-> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    let checkpoint = checkpoint(parent.path()).await?;
    let recorded = checkpoint.recorded();
    drop(checkpoint);
    let interrupted = interrupted(parent.path())?;
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
    assert_eq!(recoverable(parent.path()), []);
    Ok(())
}

#[tokio::test]
async fn cancelled_or_failed_recovery_and_cancelled_publication_keep_the_checkpoint()
-> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    drop(checkpoint(parent.path()).await?);
    let interrupted = interrupted(parent.path())?;
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
    let captured = Footage::from(checkpoint.replay()?);
    assert!(matches!(
        publish_with(
            Handoff::default(),
            captured,
            Origin::Live {
                checkpoint: Some(checkpoint)
            },
            &revoked,
            || bail!("Cancelled publication prepared storage"),
            uncopied,
        )?,
        Published::Cancelled
    ));
    assert!(interrupted.directory().exists());
    Ok(())
}

fn recover_with(
    interrupted: Interrupted,
    permit: &InsertPermit,
    parent: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Published> {
    let reopened = reopen_with(&interrupted)?;
    assert!(reopened.speech.is_none());
    publish_with(
        Handoff::default(),
        reopened.footage,
        Origin::Recovered {
            checkpoint: reopened.checkpoint,
            pending: reopened.pending,
        },
        permit,
        parent,
        uncopied,
    )
}

mod audio;
mod movie;
