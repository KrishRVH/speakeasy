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

        fn set_text(&mut self, _: &str, _: &InsertPermit) -> anyhow::Result<Option<Window>> {
            bail!("Focus accessed clipboard contents")
        }

        fn owner(&self) -> anyhow::Result<Window> {
            bail!("Focus accessed clipboard ownership")
        }

        fn holds_text(&mut self, _: &str) -> anyhow::Result<bool> {
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
        result = &mut target => bail!("Focus did not block: {result:?}"),
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

        fn set_text(&mut self, _: &str, _: &InsertPermit) -> anyhow::Result<Option<Window>> {
            self.0.send(())?;
            Ok(Some(7))
        }

        fn owner(&self) -> anyhow::Result<Window> {
            Ok(7)
        }

        fn holds_text(&mut self, _: &str) -> anyhow::Result<bool> {
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
    assert!(!previous.active());
    let current = input.begin().context("Current recording")?;
    release.send(())?;
    ready.await?;

    assert!(previous_copy.await?.is_none());
    assert_eq!(
        worker.set_text("fixture".into(), current.clone()).await?,
        Some(7)
    );
    assert_eq!(copies.try_iter().count(), 1);
    assert_eq!(
        worker
            .paste_target(7, "fixture".into(), current.clone())
            .await?,
        (false, true)
    );
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

#[tokio::test]
async fn paste_needs_its_own_text_from_the_same_owner_after_looking_up_focus() -> anyhow::Result<()>
{
    struct ManagedClipboard {
        owner: Window,
        text: &'static str,
        calls: mpsc::Sender<&'static str>,
    }

    impl ClipboardAccess for ManagedClipboard {
        fn external_target(&self, _: HostSession) -> anyhow::Result<bool> {
            self.calls.send("focus")?;
            Ok(true)
        }

        fn set_text(&mut self, _: &str, _: &InsertPermit) -> anyhow::Result<Option<Window>> {
            bail!("The selection was already written")
        }

        fn owner(&self) -> anyhow::Result<Window> {
            self.calls.send("owner")?;
            Ok(self.owner)
        }

        fn holds_text(&mut self, text: &str) -> anyhow::Result<bool> {
            self.calls.send("text")?;
            Ok(self.text == text)
        }
    }

    let (sender, _events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    for (owner, text, owned) in [
        (7, "fixture", true),
        (7, "other", false),
        (8, "fixture", false),
    ] {
        let permit = input.begin().context("Recording")?;
        let (calls, seen) = mpsc::channel();
        let worker = ClipboardWorker::spawn(input.clone(), move || {
            Ok(ManagedClipboard { owner, text, calls })
        })?;
        worker.ready().await?;
        assert_eq!(
            worker.paste_target(7, "fixture".into(), permit).await?,
            (true, owned),
            "A clipboard owned by {owner} holding {text:?} was judged wrongly"
        );
        assert_eq!(
            seen.try_iter().next(),
            Some("focus"),
            "The clipboard was checked before the focus lookup"
        );
    }
    Ok(())
}

#[test]
#[ignore = "Opt-in clipboard on a private X11 display, never the user's clipboard"]
fn an_owner_that_keeps_its_window_but_changes_its_text_no_longer_holds_the_copy()
-> anyhow::Result<()> {
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let permit = input.begin().context("Recording")?;
    let mut clipboard = Clipboard::open()?;
    let owner = clipboard
        .set_text("fixture", &permit)?
        .context("The fixture was not copied")?;
    arboard::Clipboard::new()?.set_text("other")?;
    assert_eq!(
        clipboard.owner()?,
        owner,
        "The new text took another owner window"
    );
    assert!(
        !clipboard.holds(owner, "fixture")?,
        "An owner that kept its window but changed its text still held the copy"
    );
    Ok(())
}

#[tokio::test]
async fn a_panicking_clipboard_thread_answers_its_request_and_reports_why() -> anyhow::Result<()> {
    struct PanickingClipboard;

    impl ClipboardAccess for PanickingClipboard {
        fn external_target(&self, _: HostSession) -> anyhow::Result<bool> {
            Ok(false)
        }

        fn set_text(&mut self, _: &str, _: &InsertPermit) -> anyhow::Result<Option<Window>> {
            panic::resume_unwind(Box::new("private payload"))
        }

        fn owner(&self) -> anyhow::Result<Window> {
            Ok(x11rb::NONE)
        }

        fn holds_text(&mut self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
    }

    let (events, received) = async_channel::bounded(4);
    let input = InputSender::new(events);
    let worker = ClipboardWorker::spawn(input.clone(), || Ok(PanickingClipboard))?;
    worker.ready().await?;
    let permit = input.begin().context("Recording")?;
    let answer = tokio::time::timeout(PATIENCE, worker.set_text("fixture".into(), permit))
        .await
        .context("The clipboard request was left waiting")?;
    assert!(answer.is_err());
    match tokio::time::timeout(PATIENCE, received.recv()).await? {
        Ok(Input::Unavailable(reason)) => assert!(!reason.contains("private payload")),
        other => bail!("The stopped clipboard thread did not report why: {other:?}"),
    }
    Ok(())
}
