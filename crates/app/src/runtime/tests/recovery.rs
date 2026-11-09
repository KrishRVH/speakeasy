use anyhow::Context as _;
use speakeasy_show::Take;

use super::*;
use crate::audio::fixture::square_wave;

async fn interrupted() -> anyhow::Result<(tempfile::TempDir, Interrupted)> {
    interrupted_with_audio(false).await
}

async fn interrupted_with_audio(
    keep_audio: bool,
) -> anyhow::Result<(tempfile::TempDir, Interrupted)> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let parent = root.path().to_owned();
    let show::Kept {
        keeper,
        mut frames,
        mut audio,
    } = show::Keeper::start(move || Ok(parent), now)?;
    assert!(frames.begin(
        1_000,
        Size {
            width: 80.0,
            height: 60.0
        },
        speakeasy_show::Framing::Automatic
    ));
    if keep_audio {
        assert!(audio.begin(8_000, Duration::ZERO, "fr".into()));
        assert!(
            audio.finish(
                &square_wave(3000)
                    .take(8_000)
                    .flat_map(i16::to_le_bytes)
                    .collect::<Vec<_>>(),
                &[]
            )
        );
    } else {
        drop(audio);
    }
    let mut take = Take::new(
        1_000,
        Size {
            width: 80.0,
            height: 60.0,
        },
        speakeasy_show::Framing::Automatic,
    );
    let event = take.arrive(Moment::from_millis(100), || {
        Some(Image::from_bgra(2, 2, [3, 2, 1, 255].repeat(4)).unwrap())
    })?;
    assert!(frames.send(event));
    assert!(frames.close().await);
    let checkpoint = keeper
        .close()
        .await?
        .context("Fixture checkpoint is missing")?;
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

#[tokio::test]
async fn session_owns_checkpoint_cleanup_even_when_finish_reports_are_stale() -> anyhow::Result<()>
{
    for finished in [false, true] {
        let root = tempfile::tempdir()?;
        let mut h = Harness::with_ports(Config::default(), |ports| {
            ports.checkpoint_root = Some(root.path().to_owned());
        })?;
        let (id, _, events) = start_show(&mut h).await?;
        if finished {
            finish_show(&h);
            h.phase(Phase::Processing).await?;
        }
        events.try_send(ShowEvent::Finished(id, Ok(footage())))?;
        h.runtime.request_stop();
        h.close().await?;
        let listing = show::checkpoint::list(root.path(), SystemTime::now());
        assert!(listing.problems.is_empty());
        assert_eq!(listing.shows.len(), usize::from(finished));
        assert_eq!(
            std::fs::read_dir(root.path().join(".checkpoints"))?.count(),
            usize::from(finished)
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn capture_notice_clears_on_cancel_and_never_hides_lost_narration() -> anyhow::Result<()> {
    for (notice, message) in [
        (
            ShowEvent::CheckpointLost as fn(SessionId) -> ShowEvent,
            "Recovery stopped",
        ),
        (ShowEvent::MovieStopped, "Video stopped"),
    ] {
        let (publication, publishing) = gate(1);
        let mut h = Harness::with_ports(Config::default(), |ports| {
            ports.publication = Some(publication);
        })?;
        let (id, _, events) = start_show(&mut h).await?;
        events.send(notice(id)).await?;
        h.observe(|snapshot| snapshot.message.contains(message))
            .await?;
        h.input(Input::Cancel);
        h.phase(Phase::Cancelled).await?;
        assert!(!h.snapshot().message.contains(message));
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
        events.send(notice(id)).await?;
        assert!(still_pending(h.observe(|snapshot| snapshot.message.contains(message))).await);
        publishing.release().await?;
        h.phase(Phase::Copied).await?;
        assert!(h.snapshot().message.contains("Reconnect the microphone."));
        h.close().await?;
    }
    Ok(())
}

mod audio;
