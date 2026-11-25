use std::sync::mpsc;

use anyhow::bail;
use futures_util::FutureExt as _;

use super::*;
use crate::{Input, PATIENCE};

#[tokio::test]
async fn a_blocked_focus_query_stays_off_the_caller_and_never_touches_clipboard_contents()
-> anyhow::Result<()> {
    struct FocusOnly {
        entered: async_channel::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl ClipboardAccess for FocusOnly {
        fn external_target(&self, _: HostSession) -> anyhow::Result<bool> {
            self.entered.try_send(())?;
            self.release.recv_timeout(PATIENCE)?;
            Ok(false)
        }

        fn set_text(&mut self, _: &str, _: &InsertPermit) -> anyhow::Result<bool> {
            bail!("Focus accessed clipboard contents")
        }
    }

    let (sender, _events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let permit = input.begin().context("Recording")?;
    let (entered, blocked) = async_channel::bounded(1);
    let (release, waiting) = mpsc::channel();
    let worker = ClipboardWorker::spawn(input, || {
        Ok(FocusOnly {
            entered,
            release: waiting,
        })
    })?;
    worker.ready().await?;
    let mut target = std::pin::pin!(worker.external_target(permit));
    tokio::select! {
        result = &mut target => bail!("Focus didn't block: {result:?}"),
        started = tokio::time::timeout(PATIENCE, blocked.recv()) => {
            started??;
        },
    }
    release.send(())?;
    target.await?;
    Ok(())
}

#[tokio::test]
async fn clipboard_setup_does_not_block_cancel_and_skips_a_stale_queued_copy() -> anyhow::Result<()>
{
    struct FakeClipboard(mpsc::Sender<()>);

    impl ClipboardAccess for FakeClipboard {
        fn external_target(&self, _: HostSession) -> anyhow::Result<bool> {
            Ok(false)
        }

        fn set_text(&mut self, _: &str, _: &InsertPermit) -> anyhow::Result<bool> {
            self.0.send(())?;
            Ok(true)
        }
    }

    let (events, _event_receiver) = async_channel::bounded(4);
    let input = InputSender::new(events);
    let previous = input.begin().context("Previous recording")?;
    let (copy_log, copies) = mpsc::channel();
    let (entered, opening) = async_channel::bounded(1);
    let (release, waiting) = mpsc::channel();
    let worker = ClipboardWorker::spawn(input.clone(), move || {
        entered.try_send(())?;
        waiting.recv_timeout(PATIENCE)?;
        Ok(FakeClipboard(copy_log))
    })?;
    tokio::time::timeout(PATIENCE, opening.recv()).await??;
    let mut ready = Box::pin(worker.ready());
    assert!(
        ready.as_mut().now_or_never().is_none(),
        "Desktop readiness preceded clipboard setup"
    );
    let mut previous_copy = Box::pin(worker.set_text("fixture".into(), previous.clone()));
    assert!(previous_copy.as_mut().now_or_never().is_none());
    input.deliver(Input::Cancel);
    let current = input.begin().context("Current recording")?;
    release.send(())?;
    ready.await?;

    assert!(!previous_copy.await?);
    assert!(worker.set_text("fixture".into(), current.clone()).await?);
    assert_eq!(copies.try_iter().count(), 1);
    drop(worker);
    assert!(
        !current.active(),
        "Clipboard retirement left its input generation active"
    );
    assert!(
        !input.is_closed(),
        "Clipboard retirement closed input before the desktop thread could report why"
    );
    Ok(())
}
