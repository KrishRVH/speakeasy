use std::sync::mpsc;

use anyhow::bail;
use futures_util::FutureExt as _;

use super::*;
use crate::{Input, PATIENCE};

#[test]
fn a_copy_escaped_before_its_outcome_reports_cancellation_and_a_kept_one_commits()
-> anyhow::Result<()> {
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let kept = input.begin().context("Kept recording")?;
    assert_eq!(
        settle_copy(&kept, COPIED_FOR_MANUAL_PASTE),
        COPIED_FOR_MANUAL_PASTE
    );
    assert!(!kept.active(), "A kept copy left its recording cancellable");
    let escaped = input.begin().context("Escaped recording")?;
    escaped.revoke();
    assert_eq!(
        settle_copy(&escaped, PASTE_AFTER_RELEASE),
        Inserted::Cancelled,
        "A copy reported an outcome after Escape"
    );
    Ok(())
}

#[tokio::test]
async fn readiness_waits_for_insertion_setup_and_follows_its_failure_report() -> anyhow::Result<()>
{
    let (events, received) = async_channel::bounded(4);
    let input = InputSender::new(events);
    let (inserter, requests) = crate::Inserter::lane(crate::FocusReport::default().subscribe());
    let (entered, opening) = async_channel::bounded(1);
    let (release, released) = mpsc::channel();
    let worker = InsertionWorker::spawn(
        input.clone(),
        requests,
        move || -> anyhow::Result<()> {
            entered.try_send(())?;
            released.recv_timeout(PATIENCE)?;
            bail!("Synthetic setup failure")
        },
        |(), _| {},
    )?;
    tokio::time::timeout(PATIENCE, opening.recv()).await??;
    let mut ready = Box::pin(worker.ready());
    assert!(
        ready.as_mut().now_or_never().is_none(),
        "Desktop readiness preceded insertion setup"
    );
    let permit = input.begin().context("Recording")?;
    let mut queued = Box::pin(inserter.insert(
        "fixture".into(),
        permit,
        Delivery::Paste,
        crate::Enter::Withheld,
    ));
    assert!(queued.as_mut().now_or_never().is_none());
    release.send(())?;
    assert!(
        tokio::time::timeout(PATIENCE, ready).await?.is_err(),
        "A failed setup reported readiness"
    );
    match received.try_recv() {
        Ok(Input::Unavailable(reason)) => assert!(
            reason.starts_with("X11 input is unavailable: Synthetic setup failure."),
            "{reason}"
        ),
        other => bail!("Readiness failed before setup reported why: {other:?}"),
    }
    let answer = tokio::time::timeout(PATIENCE, queued)
        .await
        .context("A queued insertion was left waiting")?;
    assert!(answer.is_err());
    Ok(())
}
