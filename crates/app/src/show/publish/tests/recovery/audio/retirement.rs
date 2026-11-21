use super::*;
use crate::show::checkpoint::AudioEnding;

const DAMAGED: &[u8] = b"NOTAUDIO and more";

async fn pending_show(parent: &Path) -> anyhow::Result<(SavedShow, Interrupted)> {
    let checkpoint = with_audio(parent).await?;
    let captured = narrated(
        Footage::from(checkpoint.replay()?),
        Narration::default(),
        Some(NarrationLoss::NotTranscribed),
    );
    let (_input, active) = permit();
    let Published::Saved { show, .. } = publish_with(
        Handoff::default(),
        captured,
        Origin::Live {
            checkpoint: Some(checkpoint),
        },
        &active,
        || Ok(parent.to_owned()),
        |_| Ok(()),
    )?
    else {
        bail!("Synthetic Show was not saved");
    };
    let pending = interrupted(parent)?;
    assert!(pending.narration_pending());
    Ok((show, pending))
}

fn retry(
    parent: &Path,
    narration: Narration,
    folder: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Published> {
    let reopened = reopen_with(interrupted(parent)?)?;
    let captured = narrated(reopened.footage, narration, None);
    let (_input, active) = permit();
    publish_with(
        Handoff::default(),
        captured,
        Origin::Recovered {
            checkpoint: reopened.checkpoint,
            pending: true,
        },
        &active,
        folder,
        uncopied,
    )
}

#[tokio::test]
async fn deleting_a_saved_show_removes_a_retry_whose_copy_never_landed() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (show, pending) = pending_show(root.path()).await?;
    let lock = root.path().join(store::retention::LOCK);
    fs::remove_file(&lock)?;
    fs::create_dir(&lock)?;
    let narration = Narration::Text("synthetic narration".into());
    assert!(retry(root.path(), narration, || Ok(root.path().to_owned())).is_err());
    fs::remove_dir(&lock)?;
    let survivor = interrupted(root.path())?;
    assert!(
        survivor.narration_pending(),
        "A copy that never landed hid the pending retry"
    );
    let removed = store::retention::delete(root.path(), show.directory())?;
    assert!(
        removed.checkpoint.is_some(),
        "Deleting the saved Show kept its private recording"
    );
    assert!(!pending.directory().exists());
    Ok(())
}

#[tokio::test]
async fn a_failed_narrated_retry_keeps_its_narration_after_its_audio_is_lost() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let (_show, pending) = pending_show(root.path()).await?;
    let narration = Narration::Text("recovered narration".into());
    assert!(
        retry(root.path(), narration.clone(), || bail!(
            "Synthetic save failure"
        ))
        .is_err()
    );
    fs::write(pending.directory().join("audio.log"), b"SPKAUD01")?;
    store::sweep(root.path(), SystemTime::now())?;
    let survivor =
        interrupted(root.path()).context("Retirement erased the only saved narration")?;
    assert!(survivor.narration_pending());
    let reopened = reopen_with(survivor)?;
    assert!(reopened.speech.is_none());
    assert_eq!(reopened.footage.recording.narration, narration);
    Ok(())
}

async fn failed_retry_with_short_audio(root: &Path) -> anyhow::Result<Interrupted> {
    let (_show, pending) = pending_show(root).await?;
    let narration = Narration::Text("recovered narration".into());
    assert!(retry(root, narration, || bail!("Synthetic save failure")).is_err());
    let donor = tempfile::tempdir()?;
    let short = with_pcm(donor.path(), &[1, 0].repeat(400), AudioEnding::Interrupted)
        .await?
        .interrupted()?;
    fs::copy(
        short.directory().join("audio.log"),
        pending.directory().join("audio.log"),
    )?;
    Ok(pending)
}

#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_saved_narration_fails_its_retry_without_erasing_it() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir()?;
    let pending = failed_retry_with_short_audio(root.path()).await?;
    let saved = pending.directory().join("narration.json");
    fs::set_permissions(&saved, fs::Permissions::from_mode(0o000))?;
    let refused = reopen_with(interrupted(root.path())?);
    assert!(saved.is_file(), "A read error erased the only transcript");
    fs::set_permissions(&saved, fs::Permissions::from_mode(0o600))?;
    let Err(error) = refused else {
        bail!("An unreadable narration was retried as missing");
    };
    assert!(format!("{error:#}").contains("Cannot read"), "{error:#}");
    let reopened = reopen_with(interrupted(root.path())?)?;
    assert_eq!(
        reopened.footage.recording.narration,
        Narration::Text("recovered narration".into())
    );
    Ok(())
}

#[tokio::test]
async fn a_damaged_saved_narration_without_speech_stays_for_explicit_discard() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let pending = failed_retry_with_short_audio(root.path()).await?;
    fs::write(pending.directory().join("narration.json"), b"{")?;
    let Err(error) = reopen_with(interrupted(root.path())?) else {
        bail!("A damaged narration was recovered");
    };
    assert!(format!("{error:#}").contains("damaged"), "{error:#}");
    let survivor = interrupted(root.path()).context("A damaged narration retired its retry")?;
    survivor.lock()?.discard()?;
    assert!(!pending.directory().exists());
    Ok(())
}

#[tokio::test]
async fn a_retry_that_hears_no_words_keeps_its_retry_without_a_duplicate() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (show, pending) = pending_show(root.path()).await?;
    for unheard in [Narration::default(), Narration::Text(" … ".into())] {
        let Err(error) = retry(root.path(), unheard, || Ok(root.path().to_owned())) else {
            bail!("A retry without words was announced as a narrated copy");
        };
        assert!(format!("{error:#}").contains("could not be transcribed"));
    }
    let saved: Vec<_> = fs::read_dir(root.path())?
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .collect();
    assert_eq!(saved, [show.directory().to_owned()]);
    assert!(!pending.directory().join("narration.json").exists());
    assert!(interrupted(root.path())?.narration_pending());
    Ok(())
}

#[tokio::test]
async fn a_pending_show_whose_audio_lost_every_sample_retires_its_retry() -> anyhow::Result<()> {
    for content in [b"".as_slice(), b"SPKAUD01"] {
        let root = tempfile::tempdir()?;
        let (show, pending) = pending_show(root.path()).await?;
        fs::write(pending.directory().join("audio.log"), content)?;
        let listing = store::checkpoint::list(root.path(), SystemTime::now());
        assert!(listing.shows.is_empty() && listing.problems.is_empty());
        store::sweep(root.path(), SystemTime::now())?;
        assert!(
            !pending.directory().exists(),
            "An unretryable Show stayed pending"
        );
        assert!(show.directory().exists());
    }
    Ok(())
}

#[tokio::test]
async fn a_pending_show_with_damaged_audio_stays_reported_and_discardable() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (show, pending) = pending_show(root.path()).await?;
    fs::write(pending.directory().join("audio.log"), DAMAGED)?;
    store::sweep(root.path(), SystemTime::now())?;
    let listing = store::checkpoint::list(root.path(), SystemTime::now());
    assert_eq!(listing.shows, []);
    let [problem] = listing.problems.as_slice() else {
        bail!("Damaged audio was not reported");
    };
    assert_eq!(problem.directory, pending.directory());
    assert!(problem.message.contains("damaged"), "{}", problem.message);
    let by_id = store::checkpoint::resolve(root.path(), Path::new(&pending.id()))?;
    let by_path = store::checkpoint::resolve(root.path(), pending.directory())?;
    assert_eq!(by_id, by_path);
    by_id.lock()?.discard()?;
    assert!(!pending.directory().exists());
    assert!(show.directory().exists());
    Ok(())
}

#[tokio::test]
async fn a_pending_show_whose_audio_header_is_wrong_is_reported_rather_than_retired()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (_show, pending) = pending_show(root.path()).await?;
    fs::write(pending.directory().join("audio.log"), b"NOTAUDIO")?;
    store::sweep(root.path(), SystemTime::now())?;
    assert!(
        pending.directory().exists(),
        "Damaged audio retired silently"
    );
    let listing = store::checkpoint::list(root.path(), SystemTime::now());
    assert_eq!(listing.problems.len(), 1);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_pending_show_whose_audio_denies_reads_stays_discardable() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir()?;
    let (show, pending) = pending_show(root.path()).await?;
    let audio = pending.directory().join("audio.log");
    fs::set_permissions(&audio, fs::Permissions::from_mode(0o000))?;
    let listing = store::checkpoint::list(root.path(), SystemTime::now());
    assert_eq!(listing.problems.len(), 1);
    let discarded = store::checkpoint::resolve(root.path(), Path::new(&pending.id()))
        .and_then(|interrupted| interrupted.lock()?.discard());
    if audio.exists() {
        fs::set_permissions(&audio, fs::Permissions::from_mode(0o600))?;
    }
    discarded?;
    assert!(!pending.directory().exists());
    assert!(show.directory().exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_saved_show_keeps_its_recovery_until_its_folder_is_synced() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir()?;
    let interrupted = with_audio(root.path()).await?.interrupted()?;
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o300))?;
    let saved = save_live(
        root.path(),
        interrupted.lock()?,
        Narration::Text("synthetic narration".into()),
        None,
    );
    let unsynced = store::checkpoint::list(root.path(), SystemTime::now());
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
    let show = saved?;
    assert!(
        interrupted.directory().exists(),
        "Recovery retired before the saved Show's folder was synced"
    );
    assert_eq!(unsynced.shows, []);
    assert_eq!(
        unsynced.problems.len(),
        1,
        "Cleanup retired an unsynced Show"
    );
    assert!(show.directory().exists());
    assert_eq!(recoverable(root.path()), []);
    store::sweep(root.path(), SystemTime::now())?;
    assert!(!interrupted.directory().exists());
    Ok(())
}

#[tokio::test]
async fn damaged_audio_does_not_block_saving_intact_frames_and_narration() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let interrupted = with_audio(root.path()).await?.interrupted()?;
    fs::write(interrupted.directory().join("audio.log"), DAMAGED)?;
    let show = save_live(
        root.path(),
        interrupted.lock()?,
        Narration::Text("synthetic narration".into()),
        None,
    )?;
    assert!(matches!(
        saved_voice(show.directory())?.status(),
        store::voice::Status::Absent
    ));
    assert_eq!(recoverable(root.path()), []);
    Ok(())
}

#[tokio::test]
async fn digital_silence_recovered_after_a_failed_save_stays_silent_without_voice()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let interrupted = with_pcm(root.path(), &[0; 16_000], AudioEnding::Complete)
        .await?
        .interrupted()?;
    let unwritable = interrupted.directory().join("narration.json");
    fs::create_dir(&unwritable)?;
    let checkpoint = interrupted.lock()?;
    let captured = narrated(
        Footage::from(checkpoint.replay()?),
        Narration::default(),
        Some(NarrationLoss::AudioSilent),
    );
    let (_input, active) = permit();
    assert!(
        publish_with(
            Handoff::default(),
            captured,
            Origin::Live {
                checkpoint: Some(checkpoint)
            },
            &active,
            || bail!("Synthetic save failure"),
            uncopied,
        )
        .is_err()
    );
    fs::remove_dir(&unwritable)?;
    let reopened = reopen_with(interrupted)?;
    assert_eq!(
        reopened.footage.recording.narration_lost,
        Some(NarrationLoss::AudioSilent),
        "Recovered digital silence lost its silence notice"
    );
    let (_input, active) = permit();
    let Published::Recovered { show, .. } = publish_with(
        Handoff::default(),
        reopened.footage,
        Origin::Recovered {
            checkpoint: reopened.checkpoint,
            pending: reopened.pending,
        },
        &active,
        || Ok(root.path().to_owned()),
        uncopied,
    )?
    else {
        bail!("Silent Show was not recovered");
    };
    assert!(matches!(
        saved_voice(show.directory())?.status(),
        store::voice::Status::Absent
    ));
    Ok(())
}
