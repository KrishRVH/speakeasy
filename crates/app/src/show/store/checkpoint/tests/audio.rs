//! Durable narration precedes erasure; failed deletion keeps cleanup's authority and retry path.

use super::*;

async fn audio_checkpoint(parent: &Path) -> anyhow::Result<Checkpoint> {
    let parent = parent.to_owned();
    let Kept {
        keeper,
        frames,
        mut audio,
    } = Keeper::start(move || Ok(parent), SystemTime::now())?;
    drop(frames);
    assert!(audio.begin(8_000, std::time::Duration::ZERO, "en".into()));
    assert!(audio.finish(&[1, 0, 2, 0], &[]));
    keeper.close().await
}

#[tokio::test]
async fn narration_write_failure_keeps_audio_and_unlink_failure_keeps_durable_narration()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = audio_checkpoint(root.path()).await?;
    let path = checkpoint.directory.clone();
    fs::create_dir(path.join(NARRATION))?;
    assert!(checkpoint.narrate(&Narration::default(), false).is_err());
    assert_eq!(&*checkpoint.audio()?.unwrap().pcm, &[1, 0, 2, 0]);
    fs::remove_dir(path.join(NARRATION))?;
    let held = root.path().join("held-audio");
    fs::rename(path.join(super::super::audio::FILE), &held)?;
    fs::create_dir(path.join(super::super::audio::FILE))?;
    let narration = Narration::Text("synthetic successful transcription".into());
    assert!(checkpoint.narrate(&narration, true).is_err());
    assert_eq!(checkpoint.narration()?, Some((narration, true)));
    fs::remove_dir(path.join(super::super::audio::FILE))?;
    fs::rename(held, path.join(super::super::audio::FILE))?;
    checkpoint.narrate(&Narration::default(), false)?;
    assert!(!checkpoint.has_audio()?);
    Ok(())
}

#[tokio::test]
async fn failed_audio_discard_keeps_the_lock_and_marker_until_cleanup_can_retry()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = audio_checkpoint(root.path()).await?;
    let path = checkpoint.directory.clone();
    let held = root.path().join("held-audio");
    fs::rename(path.join(super::super::audio::FILE), &held)?;
    fs::create_dir(path.join(super::super::audio::FILE))?;
    assert!(checkpoint.discard().is_err());
    assert!(path.join(LOCK).is_file());
    assert!(path.join(DISCARDED).is_file());
    assert_eq!(list(root.path(), SystemTime::now()).shows, []);
    fs::remove_dir(path.join(super::super::audio::FILE))?;
    fs::rename(held, path.join(super::super::audio::FILE))?;
    sweep(root.path(), SystemTime::now())?;
    assert!(!path.exists());
    Ok(())
}
