use super::*;

#[tokio::test]
async fn durable_voiced_publication_retires_checkpoint_after_a_crash_before_discard()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = with_audio(root.path()).await?;
    let captured = narrated(
        Footage::from(checkpoint.replay()?),
        Narration::Text("synthetic narration".into()),
        None,
    );
    let (_input, active) = permit();
    let saved = persist(
        Handoff::default(),
        captured,
        Some(&checkpoint),
        Completion::Narrated,
        &active,
        || Ok(root.path().to_owned()),
    )?
    .unwrap();
    assert!(checkpoint.has_audio()?);
    assert!(!checkpoint.pending()?);
    let path = checkpoint.interrupted()?.directory().to_owned();
    assert_eq!(
        store::checkpoint::list(root.path(), SystemTime::now()).shows,
        []
    );
    let voice = store::voice::read(saved.show.directory(), SystemTime::now())?;
    assert!(matches!(voice.status(), store::voice::Status::Available));
    assert_eq!(voice.audio().unwrap().pcm.len(), 16_000);
    store::sweep(root.path(), SystemTime::now())?;
    assert!(!path.exists());
    assert!(saved.show.directory().join("voice.log").exists());
    Ok(())
}

#[tokio::test]
async fn failed_or_cancelled_narrated_copy_keeps_new_narration_recoverable() -> anyhow::Result<()> {
    for cancelled in [false, true] {
        let root = tempfile::tempdir()?;
        let checkpoint = with_audio(root.path()).await?;
        let captured = narrated(
            Footage::from(checkpoint.replay()?),
            Narration::default(),
            Some(NarrationLoss::NotTranscribed),
        );
        let (_input, active) = permit();
        assert!(matches!(
            publish_with(
                Handoff::default(),
                captured,
                Origin::Live {
                    checkpoint: Some(checkpoint)
                },
                &active,
                || Ok(root.path().to_owned()),
                |_| Ok(())
            )?,
            Published::Saved {
                preservation: Preservation {
                    audio_kept: true,
                    ..
                },
                ..
            }
        ));
        let pending = store::checkpoint::list(root.path(), SystemTime::now())
            .shows
            .remove(0);
        let reopened = reopen_with(pending)?;
        let narration = Narration::Text("successfully recovered narration".into());
        let captured = narrated(reopened.footage, narration.clone(), None);
        let (input, active) = permit();
        if cancelled {
            input.cancel();
        }
        let result = publish_with(
            Handoff::default(),
            captured,
            Origin::Recovered {
                checkpoint: reopened.checkpoint,
                pending: true,
            },
            &active,
            || bail!("Synthetic save failure"),
            uncopied,
        );
        if cancelled {
            assert!(matches!(result?, Published::Cancelled));
        } else {
            assert!(result.is_err());
        }
        let mut listing = store::checkpoint::list(root.path(), SystemTime::now());
        assert_eq!(
            listing.shows.len(),
            1,
            "Old publication marker hid the new narration"
        );
        let reopened = reopen_with(listing.shows.remove(0))?;
        assert!(reopened.speech.is_none());
        assert_eq!(reopened.footage.recording.narration, narration);
        assert!(reopened.checkpoint.has_audio()?);
    }
    Ok(())
}

#[tokio::test]
async fn durable_bundle_retires_checkpoint_even_when_narration_checkpoint_write_fails()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let interrupted = with_audio(root.path()).await?.interrupted()?;
    fs::create_dir(interrupted.directory().join("narration.json"))?;
    let checkpoint = interrupted.lock()?;
    let captured = narrated(
        Footage::from(checkpoint.replay()?),
        Narration::Text("already transcribed".into()),
        None,
    );
    let (_input, active) = permit();
    assert!(matches!(
        publish_with(
            Handoff::default(),
            captured,
            Origin::Live {
                checkpoint: Some(checkpoint)
            },
            &active,
            || Ok(root.path().to_owned()),
            |_| Ok(())
        )?,
        Published::Saved {
            preservation: Preservation {
                audio_kept: false,
                ..
            },
            ..
        }
    ));
    assert_eq!(
        store::checkpoint::list(root.path(), SystemTime::now()).shows,
        []
    );
    assert!(!interrupted.directory().join("audio.log").exists());
    Ok(())
}

#[tokio::test]
async fn only_audio_that_may_hold_speech_is_kept_for_retry_or_as_voice() -> anyhow::Result<()> {
    for (lost, retry, voiced) in [
        (None, false, true),
        (Some(NarrationLoss::AudioStopped), true, false),
        (Some(NarrationLoss::NotTranscribed), true, false),
        (Some(NarrationLoss::AudioSilent), false, false),
    ] {
        let root = tempfile::tempdir()?;
        let checkpoint = with_audio(root.path()).await?;
        let captured = narrated(
            Footage::from(checkpoint.replay()?),
            Narration::default(),
            lost,
        );
        let (_input, active) = permit();
        let Published::Saved {
            show, preservation, ..
        } = publish_with(
            Handoff::default(),
            captured,
            Origin::Live {
                checkpoint: Some(checkpoint),
            },
            &active,
            || Ok(root.path().to_owned()),
            |_| Ok(()),
        )?
        else {
            bail!("The {lost:?} Show was not saved");
        };
        assert_eq!(preservation.audio_kept, retry, "{lost:?}");
        assert_eq!(
            store::checkpoint::list(root.path(), SystemTime::now())
                .shows
                .len(),
            usize::from(retry),
            "{lost:?}"
        );
        let voice = store::voice::read(show.directory(), SystemTime::now())?;
        assert_eq!(
            matches!(voice.status(), store::voice::Status::Available),
            voiced,
            "{lost:?}"
        );
    }
    Ok(())
}

fn save_live(
    parent: &Path,
    checkpoint: Checkpoint,
    narration: Narration,
    loss: Option<NarrationLoss>,
) -> anyhow::Result<SavedShow> {
    let captured = narrated(Footage::from(checkpoint.replay()?), narration, loss);
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
    Ok(show)
}

#[tokio::test]
async fn a_lost_narrated_copy_keeps_the_pending_bundle_link_deletable() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = with_audio(root.path()).await?;
    let show = save_live(
        root.path(),
        checkpoint,
        Narration::default(),
        Some(NarrationLoss::NotTranscribed),
    )?;
    let mut listing = store::checkpoint::list(root.path(), SystemTime::now());
    let reopened = reopen_with(listing.shows.remove(0))?;
    let checkpoint = reopened.checkpoint;
    let captured = narrated(
        reopened.footage,
        Narration::Text("synthetic narration".into()),
        None,
    );
    let (_input, active) = permit();
    assert!(
        publish_with(
            Handoff::default(),
            captured,
            Origin::Recovered {
                checkpoint,
                pending: true
            },
            &active,
            || bail!("Synthetic save failure"),
            uncopied,
        )
        .is_err()
    );
    let removed = store::retention::delete(root.path(), show.directory())?;
    assert!(removed.checkpoint.is_some());
    assert!(!show.directory().exists());
    Ok(())
}

#[tokio::test]
async fn an_audio_file_without_samples_does_not_block_publication_or_become_voice()
-> anyhow::Result<()> {
    for content in [b"".as_slice(), b"SPKAUD01"] {
        let root = tempfile::tempdir()?;
        let checkpoint = with_audio(root.path()).await?;
        let interrupted = checkpoint.interrupted()?;
        fs::write(interrupted.directory().join("audio.log"), content)?;
        let checkpoint = interrupted.lock()?;
        let show = save_live(
            root.path(),
            checkpoint,
            Narration::Text("synthetic narration".into()),
            None,
        )?;
        let voice = store::voice::read(show.directory(), SystemTime::now())?;
        assert!(matches!(voice.status(), store::voice::Status::Absent));
    }
    Ok(())
}

#[tokio::test]
async fn failed_silent_show_recovery_preserves_the_silence_notice_without_voice()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = with_audio(root.path()).await?;
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
    let mut listing = store::checkpoint::list(root.path(), SystemTime::now());
    let reopened = reopen_with(listing.shows.remove(0))?;
    assert_eq!(
        reopened.footage.recording.narration_lost,
        Some(NarrationLoss::AudioSilent)
    );
    assert!(reopened.speech.is_none());
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
        store::voice::read(show.directory(), SystemTime::now())?.status(),
        store::voice::Status::Absent
    ));
    Ok(())
}

#[tokio::test]
async fn a_pending_show_without_sealable_speech_cannot_publish_an_unnarrated_copy()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = with_pcm(
        root.path(),
        &[1, 0, 2, 0],
        crate::show::checkpoint::AudioEnding::Interrupted,
    )
    .await?;
    let show = save_live(
        root.path(),
        checkpoint,
        Narration::default(),
        Some(NarrationLoss::AudioStopped),
    )?;
    let mut listing = store::checkpoint::list(root.path(), SystemTime::now());
    let reopened = reopen_with(listing.shows.remove(0))?;
    assert!(reopened.speech.is_none());
    let (_input, active) = permit();
    assert!(
        publish_with(
            Handoff::default(),
            reopened.footage,
            Origin::Recovered {
                checkpoint: reopened.checkpoint,
                pending: reopened.pending
            },
            &active,
            || panic!("An unnarrated copy was staged"),
            uncopied,
        )
        .is_err()
    );
    assert!(show.directory().exists());
    assert_eq!(
        store::checkpoint::list(root.path(), SystemTime::now())
            .shows
            .len(),
        1
    );
    Ok(())
}
