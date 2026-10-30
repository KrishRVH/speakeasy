//! Durable frame handoff uses synthetic capture; the session owns checkpoint disposition.
use std::time::SystemTime;

use super::*;
use crate::show::{Keeper, Kept};

#[tokio::test(start_paused = true)]
async fn capture_finish_flushes_end_before_handoff() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (keeper, log) = Keeper::fixture(
        root.path(),
        SystemTime::now(),
        JOURNAL_BUDGET,
        Size {
            width: 1_000.0,
            height: 500.0,
        },
    )?;
    let (footage, _) = record_with(
        |take| take.log = Some(log),
        async |feed| {
            feed.frame(0).await;
            wait(10).await;
            feed.finish().await;
        },
    )
    .await;
    assert!(footage?.is_some());
    let checkpoint = keeper.close().await?;
    assert!(checkpoint.replay()?.ended());
    checkpoint.discard()?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn invalid_late_events_keep_the_frames_and_checkpoint_already_recorded() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let (checkpoint, writer) = Keeper::fixture(
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
        checkpoint
            .close()
            .await?
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
    let Kept {
        keeper,
        frames: writer,
        audio,
    } = crate::show::Keeper::start(
        || anyhow::bail!("Synthetic checkpoint failure"),
        SystemTime::now(),
    )?;
    drop(audio);
    let (events, notices) = async_channel::bounded(4);
    let (recorded, _) = record_with(
        |take| {
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
    assert!(keeper.close().await.is_err());
    let footage = recorded?.context("Recording survives checkpoint failure")?;
    assert!(footage.journal.samples().len() >= 2);
    assert_eq!(footage.recording.ending, Ending::Finished);
    assert!(
        notices.try_recv().is_err(),
        "Final flush repeated the notice"
    );
    Ok(())
}
