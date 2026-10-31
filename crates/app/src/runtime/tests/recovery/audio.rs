//! Recovery speech uses the existing worker and never opens capture devices.

use super::*;

#[tokio::test(start_paused = true)]
async fn recovery_uses_original_language_and_current_cleanup_without_capture_or_paste()
-> anyhow::Result<()> {
    let (_root, interrupted) = interrupted_with_audio(true).await?;
    let config = Config {
        language: "en".into(),
        vocabulary: vec![crate::transcript::vocabulary::Correction {
            heard: "synthetic".into(),
            written: "corrected".into(),
        }],
        ..Config::default()
    };
    let mut h = Harness::with_ports(config, |_| {})?;
    let receipt = h.runtime.recover(interrupted).map_err(anyhow::Error::msg)?;
    let (language, reply) = receive(&h.jobs).await?;
    assert_eq!(language, "fr");
    answer(reply, "synthetic um recovered narration")?;
    let saved = receive(&receipt).await?.map_err(anyhow::Error::msg)?;
    h.phase(Phase::Notice).await?;
    assert_eq!(
        saved,
        saved_show(&Narration::Text("corrected um recovered narration".into()))
    );
    assert_eq!(h.snapshot().last_show, Some(saved));
    assert!(h.captures.try_recv().is_err());
    assert!(h.shows.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    assert!(h.published.try_recv().is_err());
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    h.input(Input::Release);
    let (language, reply) = receive(&h.jobs).await?;
    assert_eq!(
        language, "en",
        "Recovery changed the configured dictation language"
    );
    answer(reply, "ordinary dictation")?;
    h.phase(Phase::Done).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn quit_during_recovery_transcription_retains_audio_and_releases_its_lock()
-> anyhow::Result<()> {
    let (_root, interrupted) = interrupted_with_audio(true).await?;
    let h = Harness::new()?;
    let _receipt = h
        .runtime
        .recover(interrupted.clone())
        .map_err(anyhow::Error::msg)?;
    let (_, reply) = receive(&h.jobs).await?;
    h.runtime.request_stop();
    answer(reply, "obsolete recovery")?;
    h.close().await?;
    let checkpoint = interrupted.lock()?;
    assert!(checkpoint.audio()?.is_some());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn ordinary_dictation_never_creates_a_show_checkpoint() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.checkpoint_root = Some(root.path().to_owned());
    })?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    h.input(Input::Release);
    h.transcribe_next_as("ordinary dictation").await?;
    h.phase(Phase::Done).await?;
    h.close().await?;
    assert_eq!(std::fs::read_dir(root.path())?.count(), 0);
    Ok(())
}

// This keeper uses an OS thread; virtual time can expire while its real file I/O is pending.
#[tokio::test]
async fn finishing_without_choosing_an_area_discards_the_session_checkpoint() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.checkpoint_root = Some(root.path().to_owned());
        ports.screen_unchosen = Some(Unchosen::Area);
    })?;
    start_show(&mut h).await?;
    finish_show(&h);
    h.phase(Phase::Unchosen).await?;
    h.close().await?;
    assert_eq!(
        std::fs::read_dir(root.path().join(".checkpoints"))?.count(),
        0
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn reopening_failure_reaches_the_library_receipt_with_its_remedy() -> anyhow::Result<()> {
    let (_root, interrupted) = interrupted_with_audio(true).await?;
    std::fs::remove_file(interrupted.directory().join("take.log"))?;
    let mut h = Harness::new()?;
    let receipt = h
        .runtime
        .recover(interrupted.clone())
        .map_err(anyhow::Error::msg)?;
    let message = receive(&receipt).await?.unwrap_err();
    assert!(message.starts_with("Cannot recover this Show:"));
    assert!(message.contains("No complete frame survived. Discard this interrupted Show."));
    h.phase(Phase::Error).await?;
    h.close().await?;
    assert!(interrupted.lock()?.audio()?.is_some());
    Ok(())
}

#[tokio::test]
async fn a_finished_show_keeps_an_unwind_but_discards_capture_failures() -> anyhow::Result<()> {
    for unwound in [false, true] {
        let root = tempfile::tempdir()?;
        let mut h = Harness::with_ports(Config::default(), |ports| {
            ports.checkpoint_root = Some(root.path().to_owned());
        })?;
        let (id, _, events) = start_show(&mut h).await?;
        finish_show(&h);
        // Inputs outrank reports, so finish takes effect before this failed report. A later
        // successful fake report is stale and cannot decide the checkpoint's disposition.
        let error = if unwound {
            anyhow!(crate::show::Unwound)
        } else {
            anyhow!("Synthetic screen permission failure")
        };
        events.try_send(ShowEvent::Finished(id, Err(error)))?;
        h.phase(Phase::Error).await?;
        h.close().await?;
        let listing = crate::show::checkpoint::list(root.path(), std::time::SystemTime::now());
        assert_eq!(listing.shows.len(), usize::from(unwound));
    }
    Ok(())
}
