use super::*;
use crate::show::store::checkpoint::audio::FILE as AUDIO;

async fn audio_checkpoint(parent: &Path) -> anyhow::Result<Checkpoint> {
    let parent = parent.to_owned();
    let Kept {
        keeper,
        frames,
        mut audio,
    } = Keeper::start(move || Ok(parent), SystemTime::now())?;
    drop(frames);
    assert!(audio.begin(8_000, std::time::Duration::ZERO, "en".into()));
    assert!(audio.finish(&[1, 0, 2, 0], &[], AudioEnding::Complete));
    keeper
        .close()
        .await?
        .context("Fixture checkpoint is missing")
}

#[tokio::test]
async fn narration_write_failure_keeps_audio_and_success_keeps_it_until_bundle_publication()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = audio_checkpoint(root.path()).await?;
    let path = checkpoint.directory.clone();
    fs::create_dir(path.join(NARRATION))?;
    assert!(checkpoint.narrate(&Narration::default(), None).is_err());
    assert_eq!(&*checkpoint.audio()?.unwrap().pcm, &[1, 0, 2, 0]);
    fs::remove_dir(path.join(NARRATION))?;
    let held = root.path().join("held-audio");
    fs::rename(path.join(AUDIO), &held)?;
    fs::create_dir(path.join(AUDIO))?;
    let narration = Narration::Text("synthetic successful transcription".into());
    checkpoint.narrate(&narration, Some(NarrationLoss::Incomplete))?;
    assert_eq!(
        checkpoint.narration()?,
        SavedNarration::Saved(narration, Some(NarrationLoss::Incomplete))
    );
    fs::remove_dir(path.join(AUDIO))?;
    fs::rename(held, path.join(AUDIO))?;
    checkpoint.narrate(&Narration::default(), None)?;
    assert!(checkpoint.has_audio()?);
    Ok(())
}

#[tokio::test]
async fn failed_audio_discard_keeps_the_lock_and_marker_until_cleanup_can_retry()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = audio_checkpoint(root.path()).await?;
    let path = checkpoint.directory.clone();
    let held = root.path().join("held-audio");
    fs::rename(path.join(AUDIO), &held)?;
    fs::create_dir(path.join(AUDIO))?;
    assert!(checkpoint.discard().is_err());
    assert!(path.join(LOCK).is_file());
    assert!(path.join(DISCARDED).is_file());
    assert_eq!(list(root.path(), SystemTime::now()).shows, []);
    fs::remove_dir(path.join(AUDIO))?;
    fs::rename(held, path.join(AUDIO))?;
    sweep(root.path(), SystemTime::now())?;
    assert!(!path.exists());
    Ok(())
}
