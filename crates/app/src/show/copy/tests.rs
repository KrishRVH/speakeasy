use std::{fs, time::Duration};

use speakeasy_platform::InputSender;

use super::*;

const PATIENCE: Duration = Duration::from_secs(5);

#[tokio::test]
async fn a_delivered_result_waits_for_native_cleanup_before_announcing_availability()
-> anyhow::Result<()> {
    let (done, result) = async_channel::bounded(1);
    let (sent, observed) = async_channel::bounded(1);
    let (release, wait) = std::sync::mpsc::channel();
    let thread = OwnedThread::spawn("copy-drain-fixture", move || {
        assert!(done.try_send(Ok(Outcome::Copied)).is_ok());
        sent.try_send(()).unwrap();
        wait.recv_timeout(PATIENCE).unwrap();
    })?;
    let mut receipt = Box::pin(completed(result, thread.exited()));
    tokio::time::timeout(PATIENCE, observed.recv()).await??;
    let early = tokio::time::timeout(Duration::from_millis(10), &mut receipt)
        .await
        .is_ok();
    release.send(())?;
    assert!(
        !early,
        "Copy success was announced before native cleanup acknowledged"
    );
    assert!(matches!(
        tokio::time::timeout(PATIENCE, receipt)
            .await?
            .map_err(anyhow::Error::msg)?,
        Outcome::Copied
    ));
    Ok(())
}

#[tokio::test]
async fn cancellation_receipt_loss_and_new_recordings_revoke_blocked_copies() -> anyhow::Result<()>
{
    for end in 0..3 {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("summary.md"), "PUBLIC_COPY_FIXTURE")?;
        let (sender, _events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let mut copies = Copies::default();
        let (reached, acquired) = async_channel::bounded(1);
        let (release, waiting) = std::sync::mpsc::channel();
        let (published, written) = async_channel::bounded(1);
        let mut receipt = Some(copies.start(
            Request::Summary(root.path().to_owned()),
            input.idle(),
            move |_, authorized| {
                reached.try_send(())?;
                waiting.recv_timeout(PATIENCE)?;
                let allowed = authorized();
                published.try_send(allowed)?;
                Ok(allowed)
            },
        )?);
        tokio::time::timeout(PATIENCE, acquired.recv()).await??;
        match end {
            0 => copies.cancel(),
            1 => receipt = None,
            _ => assert!(input.begin().unwrap().commit()),
        }
        let busy = copies.is_busy();
        let early = tokio::time::timeout(Duration::from_millis(10), copies.drained())
            .await
            .is_ok();
        let replacement = copies.start(Request::Summary(root.path().to_owned()), None, |_, _| {
            Ok(true)
        });
        release.send(())?;
        tokio::time::timeout(PATIENCE, copies.drained()).await?;
        assert!(
            busy && !early,
            "A cancelled native operation stopped being owned before acknowledging"
        );
        assert!(
            replacement.is_err(),
            "A replacement overtook blocked native work"
        );
        assert!(
            !written.recv().await?,
            "A stale request copied after cancellation or newer dictation"
        );
        if let Some(receipt) = receipt {
            assert!(receipt.await.is_err());
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_missing_saved_show_does_not_copy_another_show_or_report_success() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let mut copies = Copies::default();
    let receipt = copies.start(
        Request::Saved(super::super::SavedShow::assume(root.path().join("missing"))),
        None,
        |_, _| anyhow::bail!("A missing Show touched the clipboard"),
    )?;
    assert!(matches!(
        tokio::time::timeout(PATIENCE, receipt)
            .await?
            .map_err(anyhow::Error::msg)?,
        Outcome::Missing
    ));
    copies.drained().await;
    Ok(())
}

#[tokio::test]
async fn native_failure_reaches_the_requester_and_the_next_copy_can_run() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("summary.md"), "PUBLIC_COPY_FIXTURE")?;
    let mut copies = Copies::default();
    let receipt = copies.start(Request::Summary(root.path().to_owned()), None, |_, _| {
        anyhow::bail!("Clipboard is held")
    })?;
    let error = tokio::time::timeout(PATIENCE, receipt)
        .await?
        .err()
        .unwrap();
    assert!(error.contains("Clipboard is held"));
    copies.drained().await;
    let receipt = copies.start(
        Request::Summary(root.path().to_owned()),
        None,
        |_, authorized| Ok(authorized()),
    )?;
    assert!(matches!(
        tokio::time::timeout(PATIENCE, receipt)
            .await?
            .map_err(anyhow::Error::msg)?,
        Outcome::Copied
    ));
    copies.drained().await;
    Ok(())
}
