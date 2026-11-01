use super::*;

#[tokio::test]
async fn durable_voiced_publication_retires_checkpoint_after_a_crash_before_discard()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = with_audio(root.path()).await?;
    let captured = narrated(
        footage(&checkpoint)?,
        Narration::Text("synthetic narration".into()),
        None,
    );
    let (_input, active) = permit();
    let saved = persist(
        None,
        captured.recording,
        captured.journal,
        Some(&checkpoint),
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
    let (status, voice) = store::voice::read(saved.show.directory(), SystemTime::now())?;
    assert!(matches!(status, store::voice::Status::Available));
    assert_eq!(voice.unwrap().pcm.len(), 16_000);
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
            footage(&checkpoint)?,
            Narration::default(),
            Some(NarrationLoss::NotTranscribed),
        );
        let (_input, active) = permit();
        assert!(matches!(
            publish_with(
                None,
                captured,
                Some(checkpoint),
                Origin::Live,
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
            None,
            captured,
            Some(reopened.checkpoint),
            Origin::Recovered { pending: true },
            &active,
            || bail!("Synthetic save failure"),
            |_| bail!("Recovery copied"),
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
async fn a_saved_narrated_bundle_retires_audio_even_if_checkpoint_narration_cannot_be_written()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let interrupted = with_audio(root.path()).await?.interrupted()?;
    fs::create_dir(interrupted.directory().join("narration.json"))?;
    let checkpoint = interrupted.lock()?;
    let captured = narrated(
        footage(&checkpoint)?,
        Narration::Text("already transcribed".into()),
        None,
    );
    let (_input, active) = permit();
    assert!(matches!(
        publish_with(
            None,
            captured,
            Some(checkpoint),
            Origin::Live,
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
