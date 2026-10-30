//! Recovery shares publication authority, waits for the active session, and requests no input or speech.

use super::*;

async fn interrupted() -> anyhow::Result<(tempfile::TempDir, crate::show::Interrupted)> {
    let root = tempfile::tempdir()?;
    let now = std::time::SystemTime::now();
    let (checkpoint, writer) = crate::show::Checkpoint::begin(
        root.path(),
        now,
        1_000,
        Size {
            width: 80.0,
            height: 60.0,
        },
    )?;
    assert!(writer.close().await);
    let interrupted = checkpoint.interrupted()?;
    Ok((root, interrupted))
}

#[tokio::test(start_paused = true)]
async fn recovery_waits_for_dictation_then_saves_without_microphone_speech_or_copy()
-> anyhow::Result<()> {
    let (_root, interrupted) = interrupted().await?;
    let (recovery, recovering) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.show_recovery = Some(recovery);
    })?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    let receipt = h
        .runtime
        .recover(interrupted.clone())
        .map_err(anyhow::Error::msg)?;
    assert!(still_pending(recovering.reached()).await);
    assert!(h.runtime.recover(interrupted).is_err());
    h.input(Input::Cancel);
    h.input(Input::Release);
    let authority = recovering.reached().await?;
    h.phase(Phase::Processing).await?;
    for input in [Input::Press, Input::Escape, Input::Cancel, Input::Release] {
        h.input(input);
    }
    assert!(still_pending(h.phase(Phase::Cancelled)).await);
    assert!(authority.active());
    recovering.release().await?;
    let saved = receive(&receipt).await?.map_err(anyhow::Error::msg)?;
    h.phase(Phase::Notice).await?;
    assert_eq!(h.snapshot().last_show, Some(saved));
    assert!(h.captures.try_recv().is_err());
    assert!(h.jobs.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    assert!(h.published.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn quit_revokes_recovery_and_awaits_its_owned_cleanup() -> anyhow::Result<()> {
    let (_root, interrupted) = interrupted().await?;
    let directory = interrupted.directory().to_owned();
    let (recovery, recovering) = gate(1);
    let h = Harness::with_ports(Config::default(), |ports| {
        ports.show_recovery = Some(recovery);
    })?;
    let receipt = h.runtime.recover(interrupted).map_err(anyhow::Error::msg)?;
    let authority = recovering.reached().await?;
    h.runtime.request_stop();
    assert!(!authority.active());
    assert!(still_pending(h.exited()).await);
    recovering.release().await?;
    assert!(receive(&receipt).await?.is_err());
    h.close().await?;
    assert!(directory.exists());
    Ok(())
}

async fn checkpointed_footage()
-> anyhow::Result<(tempfile::TempDir, crate::show::Interrupted, Box<Footage>)> {
    let (root, interrupted) = interrupted().await?;
    let mut footage = footage();
    footage.checkpoint = Some(interrupted.lock()?);
    Ok((root, interrupted, footage))
}

#[tokio::test(start_paused = true)]
async fn stale_finished_reports_and_shutdown_drain_discard_their_real_checkpoints()
-> anyhow::Result<()> {
    for shutdown in [false, true] {
        let (_root, interrupted, mut footage) = checkpointed_footage().await?;
        footage.recording.ending = speakeasy_show::Ending::ScreenStopped;
        let mut h = Harness::new()?;
        let (old, _, events) = start_show(&mut h).await?;
        h.input(Input::Cancel);
        h.phase(Phase::Cancelled).await?;
        h.input(Input::Release);
        if shutdown {
            events.try_send(ShowEvent::Finished(old, Ok(footage)))?;
            h.runtime.request_stop();
        } else {
            let (current, _, current_events) = start_show(&mut h).await?;
            events.send(ShowEvent::Finished(old, Ok(footage))).await?;
            current_events
                .send(ShowEvent::CheckpointLost(current))
                .await?;
            h.observe(|snapshot| snapshot.message.contains("Recovery stopped"))
                .await?;
            assert!(h.snapshot().show);
        }
        h.close().await?;
        assert!(
            !interrupted.directory().exists(),
            "Discard cleanup did not finish"
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn quit_preserves_finished_footage_waiting_in_the_report_lane() -> anyhow::Result<()> {
    let (_root, interrupted, footage) = checkpointed_footage().await?;
    let mut h = Harness::new()?;
    let (id, _, events) = start_show(&mut h).await?;
    finish_show(&h);
    // No await between report and stop: shutdown wins the biased select and drains the report.
    events.try_send(ShowEvent::Finished(id, Ok(footage)))?;
    h.runtime.request_stop();
    h.close().await?;
    assert!(
        interrupted.lock().is_ok(),
        "Shutdown discarded a requested finish"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn quit_during_finished_show_transcription_preserves_its_checkpoint() -> anyhow::Result<()> {
    let (_root, interrupted, footage) = checkpointed_footage().await?;
    let mut h = Harness::new()?;
    let (id, _, events) = start_show(&mut h).await?;
    events.send(ShowEvent::Finished(id, Ok(footage))).await?;
    let (_, reply) = receive(&h.jobs).await?;
    h.runtime.request_stop();
    answer(reply, "synthetic narration")?;
    h.close().await?;
    assert!(interrupted.lock().is_ok(), "Quit discarded a finished Show");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn checkpoint_notice_clears_on_cancel_and_never_hides_lost_narration() -> anyhow::Result<()> {
    let (publication, publishing) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publication = Some(publication);
    })?;
    let (id, _, events) = start_show(&mut h).await?;
    events.send(ShowEvent::CheckpointLost(id)).await?;
    h.observe(|snapshot| snapshot.message.contains("Recovery stopped"))
        .await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    assert!(!h.snapshot().message.contains("Recovery stopped"));
    h.input(Input::Release);
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, microphone) = receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    let (_, _, events) = receive(&h.shows).await?;
    microphone
        .send(CaptureEvent::Finished(
            id,
            Err(anyhow!("Reconnect the microphone.")),
        ))
        .await?;
    publishing.reached().await?;
    events.send(ShowEvent::CheckpointLost(id)).await?;
    assert!(
        still_pending(h.observe(|snapshot| snapshot.message.contains("Recovery stopped"))).await
    );
    publishing.release().await?;
    h.phase(Phase::Copied).await?;
    assert!(h.snapshot().message.contains("Reconnect the microphone."));
    h.close().await
}
