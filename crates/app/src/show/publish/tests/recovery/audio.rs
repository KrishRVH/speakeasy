use super::*;
use crate::audio::fixture::square_wave;

fn saved_voice(directory: &Path) -> anyhow::Result<store::voice::Status> {
    Ok(store::voice::Status::of(
        store::voice::read(directory)?.as_ref(),
    ))
}

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
    assert!(frames.begin(1 << 20, surface, speakeasy_show::Framing::Automatic));
    let mut take = Take::new(1 << 20, surface, speakeasy_show::Framing::Automatic);
    assert!(
        frames.send(take.arrive(Moment::from_millis(100), || Some(desktop(
            80, 60, 0, 0, false
        )))?)
    );
    assert!(frames.send(take.end(Moment::from_millis(1_000), Ending::Finished)));
    drop(frames);
    let pcm = square_wave(3000)
        .take(8_000)
        .flat_map(i16::to_le_bytes)
        .collect::<Vec<_>>();
    assert!(audio.begin(8_000, Duration::from_millis(125), "fr".into()));
    audio.finish(&pcm, &[], crate::show::checkpoint::AudioEnding::Complete);
    keeper
        .close()
        .await?
        .context("Fixture checkpoint is missing")
}

fn save_live(
    parent: &Path,
    checkpoint: Checkpoint,
    narration: Narration,
    loss: Option<NarrationLoss>,
) -> anyhow::Result<(SavedShow, Preservation)> {
    let captured = narrated(Footage::from(checkpoint.replay()?), narration, loss);
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
        || Ok(parent.to_owned()),
        |_| Ok(()),
    )?
    else {
        bail!("Synthetic Show was not saved");
    };
    Ok((show, preservation))
}

#[tokio::test]
async fn a_failed_save_recovers_by_transcribing_the_kept_audio() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = with_audio(root.path()).await?;
    let captured = narrated(
        Footage::from(checkpoint.replay()?),
        Narration::Text("synthetic narration".into()),
        None,
    );
    let (_input, permit) = permit();
    assert!(
        publish_with(
            Handoff::default(),
            captured,
            Origin::Live {
                checkpoint: Some(checkpoint)
            },
            &permit,
            || bail!("Synthetic bundle failure"),
            uncopied,
        )
        .is_err()
    );
    let interrupted = interrupted(root.path())?;
    assert!(!interrupted.narration_pending());
    let reopened = reopen_with(&interrupted)?;
    let speech = reopened.speech.context("The kept audio was not offered")?;
    assert_eq!(&*speech.language, "fr");
    assert!(!speech.incomplete);
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
        let (show, preservation) = save_live(root.path(), checkpoint, Narration::default(), lost)?;
        assert_eq!(preservation.audio_kept, retry, "{lost:?}");
        assert_eq!(
            recoverable(root.path()).len(),
            usize::from(retry),
            "{lost:?}"
        );
        let voice = saved_voice(show.directory())?;
        assert_eq!(
            matches!(voice, store::voice::Status::Available),
            voiced,
            "{lost:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_narration_retry_saves_a_copy_only_when_it_hears_words() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = with_audio(root.path()).await?;
    let (original, _) = save_live(
        root.path(),
        checkpoint,
        Narration::default(),
        Some(NarrationLoss::NotTranscribed),
    )?;
    let summary = original.directory().join("summary.md");
    fs::write(&summary, b"user's edited summary")?;
    let pending = interrupted(root.path())?;
    assert!(pending.narration_pending());
    for (narration, lost) in [
        (Narration::default(), Some(NarrationLoss::NotTranscribed)),
        (Narration::Text("…".into()), None),
    ] {
        let reopened = reopen_with(&pending)?;
        assert!(reopened.pending);
        assert!(reopened.speech.is_some());
        let (_input, retry) = permit();
        assert!(
            publish_with(
                Handoff::default(),
                narrated(reopened.footage, narration, lost),
                Origin::Recovered {
                    checkpoint: reopened.checkpoint,
                    pending: true
                },
                &retry,
                || panic!("A failed retry prepared another bundle"),
                uncopied,
            )
            .is_err()
        );
    }
    let reopened = reopen_with(&pending)?;
    let (_input, retry) = permit();
    let Published::Recovered {
        show: copy,
        narrated_copy: true,
        preservation: Preservation {
            audio_kept: false, ..
        },
    } = publish_with(
        Handoff::default(),
        narrated(
            reopened.footage,
            Narration::Text("recovered speech".into()),
            None,
        ),
        Origin::Recovered {
            checkpoint: reopened.checkpoint,
            pending: true,
        },
        &retry,
        || Ok(root.path().to_owned()),
        uncopied,
    )?
    else {
        bail!("Retry did not save a narrated copy")
    };
    assert_ne!(copy.directory(), original.directory());
    assert_eq!(fs::read(&summary)?, b"user's edited summary");
    assert!(!pending.directory().exists());
    assert_eq!(
        Manifest::parse(&fs::read(copy.directory().join(Manifest::FILE))?)?
            .recording()
            .narration,
        Narration::Text("recovered speech".into())
    );
    Ok(())
}

#[tokio::test]
async fn kept_audio_expires_with_its_recording_even_when_the_show_is_kept() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = with_audio(root.path()).await?;
    let recorded = checkpoint.recorded();
    let (show, _) = save_live(
        root.path(),
        checkpoint,
        Narration::default(),
        Some(NarrationLoss::NotTranscribed),
    )?;
    store::retention::set_kept(root.path(), show.directory(), true)?;
    let pending = interrupted(root.path())?;
    store::sweep(
        root.path(),
        recorded + Duration::from_hours(168) + Duration::from_secs(1),
    )?;
    assert!(!pending.directory().exists());
    assert!(show.directory().exists());
    Ok(())
}
