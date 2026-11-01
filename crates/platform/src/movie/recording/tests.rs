use std::path::PathBuf;

use super::*;
use crate::Part;

#[tokio::test]
async fn a_cancelled_pending_movie_releases_its_lease_without_a_frame() {
    let (control, target) = crate::MovieControl::new(
        PathBuf::from("synthetic.mp4"),
        Instant::now()
            .checked_add(INTERVAL.saturating_mul(150))
            .unwrap(),
    );
    let (_stop, stopping) = async_channel::bounded(1);
    let mut movie = Movie::new(Some(target));
    control.request_stop();
    assert!(movie.poll(Instant::now(), &stopping));
    assert!(!movie.active());
    assert!(control.next().await.is_none());
}

#[tokio::test]
#[ignore = "Opt in: native movie encoder with synthetic pixels only"]
async fn a_cancelled_active_movie_releases_its_file_without_a_new_frame() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("synthetic.mp4");
    let (control, target) = crate::MovieControl::new(
        path.clone(),
        Instant::now()
            .checked_add(INTERVAL.saturating_mul(150))
            .unwrap(),
    );
    let (_stop, stopping) = async_channel::bounded(1);
    let (submitted, wrote) = async_channel::bounded(1);
    let (poll, polling) = async_channel::bounded(1);
    let native = crate::OwnedThread::spawn("synthetic movie cancellation", move || {
        let mut movie = Movie::new(Some(target));
        assert!(movie.frame(&frame(320, 240, 140), &stopping));
        submitted.send_blocking(()).unwrap();
        if polling.recv_blocking().is_ok() {
            assert!(movie.poll(Instant::now(), &stopping));
            assert!(!movie.active());
        }
    })?;
    let Some(crate::MovieEvent::Started { accepted, .. }) = control.next().await else {
        anyhow::bail!("Synthetic movie did not request its origin");
    };
    accepted.send(true).await?;
    let Some(crate::MovieEvent::Geometry {
        sample, accepted, ..
    }) = control.next().await
    else {
        anyhow::bail!("Synthetic movie did not request its content rectangle");
    };
    assert_eq!(sample, 0);
    accepted.send(true).await?;
    wrote.recv().await?;
    control.request_stop();
    poll.send(()).await?;
    native.join().await;
    assert!(matches!(
        control.next().await,
        Some(crate::MovieEvent::Finished {
            frames: 1,
            finalized: true,
            ..
        })
    ));
    assert!(control.next().await.is_none());
    std::fs::rename(path, directory.path().join("closed.mp4"))?;
    Ok(())
}

fn frame(width: u32, height: u32, shade: u8) -> Frame {
    Frame {
        at: Instant::now(),
        width,
        height,
        bgra: [shade, shade, shade, 255]
            .repeat(usize::try_from(u64::from(width).saturating_mul(u64::from(height))).unwrap()),
        part: Part::WHOLE,
    }
}
