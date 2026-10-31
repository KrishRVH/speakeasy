//! Successful inference remains recoverable until a narrated bundle commits.

use super::*;

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
                audio_kept: true,
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
        assert!(!reopened.checkpoint.has_audio()?);
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
            audio_kept: false,
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
