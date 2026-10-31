//! A blocked native read remains owned after view cancellation, before replacement and during quit.

use std::time::Duration;

use super::*;

#[tokio::test]
async fn closing_the_receipt_waits_for_cleanup_before_accepting_another_read() -> anyhow::Result<()>
{
    let mut reads = Reads::default();
    let (release, wait) = async_channel::bounded(1);
    let (observed, cancelled) = async_channel::bounded(1);
    let Work::Reading(receipt) = reads.start(move |cancelled| {
        wait.recv_blocking().unwrap();
        observed.try_send(cancelled()).unwrap();
        Err("Synthetic cancellation".into())
    })?
    else {
        anyhow::bail!("First read unexpectedly waited");
    };
    drop(receipt);
    let Work::Waiting(mut pending) = reads.start(|_| panic!("Replacement ran before cleanup"))?
    else {
        release.try_send(())?;
        anyhow::bail!("Replacement started before cleanup");
    };
    let early = tokio::time::timeout(Duration::from_millis(10), &mut pending)
        .await
        .is_ok();
    release.try_send(())?;
    pending.await;
    assert!(!early);
    assert!(cancelled.recv().await?);
    let Work::Reading(receipt) = reads.start(|_| Err("Latest request".into()))? else {
        anyhow::bail!("Read kept waiting after cleanup");
    };
    assert_eq!(receipt.await.err().as_deref(), Some("Latest request"));
    Ok(())
}

#[tokio::test]
async fn quit_cancels_a_read_even_when_the_window_keeps_its_receipt() -> anyhow::Result<()> {
    let mut reads = Reads::default();
    let (release, wait) = async_channel::bounded(1);
    let Work::Reading(receipt) = reads.start(move |cancelled| {
        wait.recv_blocking().unwrap();
        assert!(cancelled());
        Err("Stopped for quit".into())
    })?
    else {
        anyhow::bail!("First read unexpectedly waited");
    };
    reads.cancel();
    let early = tokio::time::timeout(Duration::from_millis(10), reads.drained())
        .await
        .is_ok();
    release.try_send(())?;
    reads.drained().await;
    assert!(!early);
    assert_eq!(receipt.await.err().as_deref(), Some("Stopped for quit"));
    Ok(())
}
