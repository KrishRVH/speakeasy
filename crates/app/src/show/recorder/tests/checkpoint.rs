//! Real private checkpoint lifetime around the recorder's synthetic capture and report paths.

use super::*;

async fn captured(root: &std::path::Path, cancel: bool) -> anyhow::Result<Option<Footage>> {
    let (checkpoint, writer) = Checkpoint::begin(
        root,
        SystemTime::now(),
        JOURNAL_BUDGET,
        Size {
            width: 1_000.0,
            height: 500.0,
        },
    )?;
    record_with(
        |take| {
            take.checkpoint = Some(checkpoint);
            take.log = Some(writer);
        },
        async |feed| {
            feed.frame(0).await;
            wait(10).await;
            if cancel {
                feed.finish.close();
            } else {
                feed.finish().await;
            }
        },
    )
    .await
    .0
}

#[tokio::test(start_paused = true)]
async fn capture_finish_flushes_end_before_handoff_and_cancel_removes_checkpoint()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let footage = captured(root.path(), false)
        .await?
        .context("The Show finished")?;
    let checkpoint = footage
        .checkpoint
        .as_ref()
        .context("The take checkpointed")?;
    assert!(
        checkpoint.replay()?.ended(),
        "End was not durable before footage handoff"
    );
    assert_eq!(
        store::checkpoint::list(root.path(), SystemTime::now()).shows,
        []
    );
    footage
        .checkpoint
        .context("The take checkpointed")?
        .discard()?;
    assert!(captured(root.path(), true).await?.is_none());
    assert_eq!(
        std::fs::read_dir(root.path().join(".checkpoints"))?.count(),
        0
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn unreceived_reports_preserve_finished_takes_and_discard_unfinished_takes()
-> anyhow::Result<()> {
    for (ending, closed_lane) in [
        (Ending::Finished, false),
        (Ending::Finished, true),
        (Ending::ScreenStopped, false),
        (Ending::ScreenStopped, true),
    ] {
        let root = tempfile::tempdir()?;
        let mut footage = captured(root.path(), false)
            .await?
            .context("The Show finished")?;
        footage.recording.ending = ending;
        let (canvas, _) = watch::channel(Canvas::default());
        let (finish, _pins, lanes) = lanes(&canvas);
        if closed_lane {
            finish.close();
        }
        let (events, receiver) = async_channel::bounded(1);
        receiver.close();
        let mut report = Report::new(SessionId::FIRST, events, &lanes);
        report.outcome = Ok(Ended::Recorded(Box::new(footage)));
        drop(report);
        let found = store::checkpoint::list(root.path(), SystemTime::now());
        assert!(found.problems.is_empty());
        assert_eq!(found.shows.len(), usize::from(ending == Ending::Finished));
        if let Some(interrupted) = found.shows.first() {
            assert!(
                interrupted.lock().is_ok(),
                "The unreceived report retained its lock"
            );
        }
        assert_eq!(
            std::fs::read_dir(root.path().join(".checkpoints"))?.count(),
            found.shows.len()
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn invalid_late_events_keep_the_frames_and_checkpoint_already_recorded() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let (checkpoint, writer) = Checkpoint::begin(
        root.path(),
        SystemTime::now(),
        JOURNAL_BUDGET,
        Size {
            width: 1_000.0,
            height: 500.0,
        },
    )?;
    let (recorded, _) = record_with(
        |take| {
            take.checkpoint = Some(checkpoint);
            take.log = Some(writer);
        },
        async |feed| {
            feed.frame(0).await;
            feed.pointer(360_001, 0.5, 0.5, false, false).await;
        },
    )
    .await;
    let footage = recorded?.context("The good prefix survives")?;
    assert_eq!(footage.recording.ending, Ending::ScreenStopped);
    assert_eq!(footage.journal.samples().len(), 1);
    assert_eq!(
        footage
            .checkpoint
            .as_ref()
            .unwrap()
            .replay()?
            .journal()
            .samples()
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_writer_warns_once_and_keeps_recording_in_memory() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (checkpoint, writer) = Checkpoint::begin(
        root.path(),
        SystemTime::now(),
        JOURNAL_BUDGET,
        Size {
            width: 1_000.0,
            height: 500.0,
        },
    )?;
    assert!(writer.close().await);
    // A read-only file makes the first checkpoint write fail independently of the live take.
    let file = tempfile::NamedTempFile::new_in(root.path())?;
    let writer = store::checkpoint::Writer::start(std::fs::File::open(file.path())?)?;
    let (events, notices) = async_channel::bounded(4);
    let (recorded, _) = record_with(
        |take| {
            take.checkpoint = Some(checkpoint);
            take.log = Some(writer);
            take.checkpoint_notice = Some((SessionId::FIRST, events));
        },
        async |feed| {
            let deadline = Instant::now() + Duration::from_secs(2);
            for at in 0.. {
                feed.frame(at).await;
                if let Ok(event) = notices.try_recv() {
                    assert!(matches!(event, ShowEvent::CheckpointLost(SessionId::FIRST)));
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "Writer failure did not reach the recorder"
                );
                wait(1).await;
            }
            feed.frame(3_000).await;
            wait(10).await;
            feed.finish().await;
        },
    )
    .await;
    let footage = recorded?.context("Recording survives checkpoint failure")?;
    assert!(footage.journal.samples().len() >= 2);
    assert_eq!(footage.recording.ending, Ending::Finished);
    assert!(
        notices.try_recv().is_err(),
        "Final flush repeated the notice"
    );
    Ok(())
}
