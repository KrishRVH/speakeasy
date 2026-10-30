//! Private audio survives failures and expires independently of a saved bundle.

use super::*;

async fn with_audio(parent: &Path) -> anyhow::Result<Checkpoint> {
    let parent = parent.to_owned();
    let crate::show::Kept {
        keeper,
        mut frames,
        mut audio,
    } = crate::show::Keeper::start(move || Ok(parent), SystemTime::now())?;
    let surface = Size {
        width: 80.0,
        height: 60.0,
    };
    assert!(frames.begin(1 << 20, surface));
    let mut take = Take::new(1 << 20, surface);
    assert!(
        frames.send(take.arrive(Moment::from_millis(100), || Some(desktop(
            80, 60, 0, 0, false
        )))?)
    );
    assert!(frames.send(take.end(Moment::from_millis(1_000), Ending::Finished)));
    assert!(audio.begin(8_000, Duration::from_millis(125), "fr".into()));
    assert!(audio.finish(&[0xB8, 0x0B].repeat(8_000), &[]));
    assert!(frames.close().await);
    keeper.close().await
}

#[tokio::test]
async fn narration_survives_a_failed_save_before_audio_is_erased() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = with_audio(root.path()).await?;
    let captured = narrated(
        footage(&checkpoint)?,
        Narration::Text("synthetic narration".into()),
        None,
    );
    let (_input, permit) = permit();
    assert!(
        publish_with(
            captured,
            Some(checkpoint),
            Origin::Live,
            &permit,
            || bail!("Synthetic bundle failure"),
            |_| bail!("Failed publication copied")
        )
        .is_err()
    );
    let interrupted = store::checkpoint::list(root.path(), SystemTime::now())
        .shows
        .remove(0);
    assert!(!interrupted.directory().join("audio.log").exists());
    let reopened = reopen_with(interrupted)?;
    assert!(
        reopened.speech.is_none(),
        "Durable narration should not need another engine request"
    );
    assert_eq!(
        reopened.footage.recording.narration,
        Narration::Text("synthetic narration".into())
    );
    Ok(())
}

#[tokio::test]
async fn pending_retry_preserves_edits_and_failure_does_not_publish_another_copy()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let selected = with_audio(root.path()).await?.interrupted()?;
    let checkpoint = selected.lock()?;
    let captured = narrated(
        footage(&checkpoint)?,
        Narration::default(),
        Some(NarrationLoss::NotTranscribed),
    );
    let (_input, active) = permit();
    let Published::Saved {
        show: original,
        audio_kept: true,
        ..
    } = publish_with(
        captured,
        Some(checkpoint),
        Origin::Live,
        &active,
        || Ok(root.path().to_owned()),
        |_| Ok(()),
    )?
    else {
        bail!("Audio was not retained")
    };
    let summary = original.directory().join("summary.md");
    fs::write(&summary, b"user's edited summary")?;
    let interrupted = store::checkpoint::list(root.path(), SystemTime::now())
        .shows
        .remove(0);
    assert!(interrupted.narration_pending());
    let reopened = reopen_with(selected)?;
    assert!(
        reopened.pending,
        "A stale selection forgot that frames were already published"
    );
    assert!(reopened.speech.is_some());
    let captured = narrated(
        reopened.footage,
        Narration::default(),
        Some(NarrationLoss::NotTranscribed),
    );
    let (_input, retry) = permit();
    assert!(
        publish_with(
            captured,
            Some(reopened.checkpoint),
            Origin::Recovered { pending: true },
            &retry,
            || bail!("A failed retry prepared another bundle"),
            |_| bail!("Recovery copied")
        )
        .is_err()
    );
    assert!(interrupted.directory().join("audio.log").exists());
    let reopened = reopen_with(interrupted.clone())?;
    let captured = narrated(
        reopened.footage,
        Narration::Text("recovered speech".into()),
        None,
    );
    let (_input, retry) = permit();
    let Published::Recovered {
        show: copy,
        narrated_copy: true,
        audio_kept: false,
        ..
    } = publish_with(
        captured,
        Some(reopened.checkpoint),
        Origin::Recovered { pending: true },
        &retry,
        || Ok(root.path().to_owned()),
        |_| bail!("Recovery copied"),
    )?
    else {
        bail!("Retry did not save a narrated copy")
    };
    assert_ne!(copy.directory(), original.directory());
    assert_eq!(fs::read(&summary)?, b"user's edited summary");
    assert!(!interrupted.directory().exists());
    assert_eq!(
        Manifest::parse(&fs::read(copy.directory().join(Manifest::FILE))?)?
            .recording()
            .narration,
        Narration::Text("recovered speech".into())
    );
    Ok(())
}

#[tokio::test]
async fn kept_bundle_cannot_extend_audio_expiry_and_discard_audio_leaves_it_untouched()
-> anyhow::Result<()> {
    for discard in [false, true] {
        let root = tempfile::tempdir()?;
        let checkpoint = with_audio(root.path()).await?;
        let recorded = checkpoint.recorded()?;
        let captured = narrated(
            footage(&checkpoint)?,
            Narration::default(),
            Some(NarrationLoss::NotTranscribed),
        );
        let (_input, active) = permit();
        let Published::Saved {
            show,
            audio_kept: true,
            ..
        } = publish_with(
            captured,
            Some(checkpoint),
            Origin::Live,
            &active,
            || Ok(root.path().to_owned()),
            |_| Ok(()),
        )?
        else {
            bail!("No pending audio")
        };
        store::retention::set_kept(root.path(), show.directory(), true)?;
        let summary = fs::read(show.directory().join("summary.md"))?;
        let pending = store::checkpoint::list(root.path(), SystemTime::now())
            .shows
            .remove(0);
        if discard {
            pending.lock()?.discard()?;
        } else {
            store::sweep(
                root.path(),
                recorded + Duration::from_hours(168) + Duration::from_secs(1),
            )?;
        }
        assert!(!pending.directory().exists());
        assert!(show.directory().exists());
        assert_eq!(fs::read(show.directory().join("summary.md"))?, summary);
    }
    Ok(())
}

mod publication;
