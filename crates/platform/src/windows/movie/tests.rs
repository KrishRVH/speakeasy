use std::{path::PathBuf, time::Duration};

use super::*;
use crate::Part;

#[tokio::test]
async fn a_cancelled_pending_movie_releases_its_lease_without_a_frame() {
    let (control, target) = crate::MovieControl::new(PathBuf::from("synthetic.mp4"));
    let (_stop, stopping) = async_channel::bounded(1);
    let mut movie = Movie::new(Some(target));
    control.request_stop();
    movie.poll(Instant::now(), &stopping);
    assert!(!movie.active());
    assert!(control.next().await.is_none());
}

#[tokio::test]
#[ignore = "Opt in: native Media Foundation encoder with synthetic pixels only"]
async fn a_cancelled_active_movie_releases_its_file_without_a_new_frame() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("synthetic.mp4");
    let (control, target) = crate::MovieControl::new(path.clone());
    let (_stop, stopping) = async_channel::bounded(1);
    let (submitted, wrote) = async_channel::bounded(1);
    let (poll, polling) = async_channel::bounded(1);
    let native = crate::OwnedThread::spawn("synthetic movie cancellation", move || {
        let mut movie = Movie::new(Some(target));
        assert!(movie.frame(&frame(320, 240, 140), &stopping));
        submitted.send_blocking(()).unwrap();
        if polling.recv_blocking().is_ok() {
            movie.poll(Instant::now(), &stopping);
            assert!(!movie.active());
        }
    })?;
    let Some(crate::MovieEvent::Started { accepted, .. }) = control.next().await else {
        anyhow::bail!("Synthetic movie did not request its origin");
    };
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

#[test]
fn resized_frames_clear_old_pixels_and_keep_aspect_ratio() -> anyhow::Result<()> {
    let mut canvas = canvas::Canvas::new([4, 4])?;
    canvas.paint(&frame(4, 4, 200))?;
    let pixels = canvas.paint(&frame(4, 2, 100))?;
    assert_eq!(&pixels[..16], &[0; 16]);
    assert_eq!(&pixels[16..48], &[100, 100, 100, 255].repeat(8));
    assert_eq!(&pixels[48..], &[0; 16]);
    let pixels = canvas.paint(&frame(2, 4, 75))?;
    for row in pixels.as_chunks::<16>().0 {
        assert_eq!(&row[..4], &[0; 4]);
        assert_eq!(&row[4..12], &[75, 75, 75, 255].repeat(2));
        assert_eq!(&row[12..], &[0; 4]);
    }
    let mut invalid = frame(4, 4, 0);
    invalid.bgra.pop();
    assert!(canvas.paint(&invalid).is_err());
    assert!(canvas::Canvas::new([u32::MAX, 1]).is_err());
    let mut grid = frame(8, 8, 0);
    for (index, pixel) in grid.bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let shade = if index % 2 == 0 { 0 } else { 255 };
        *pixel = [shade, shade, shade, 255];
    }
    assert!(
        canvas
            .paint(&grid)?
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| *pixel == [128, 128, 128, 255]),
        "Downsampling aliases thin lines instead of blending them"
    );
    Ok(())
}

#[test]
#[ignore = "Opt in: native Media Foundation encoder, synthetic pixels only; set SPEAKEASY_MOVIE_PROBE_DIRECTORY to an empty artifact directory"]
fn encode_synthetic_movie_fragments() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var_os("SPEAKEASY_MOVIE_PROBE_DIRECTORY")
            .ok_or_else(|| anyhow::anyhow!("Choose an explicit movie probe directory"))?,
    );
    anyhow::ensure!(directory.is_dir(), "Movie probe directory must exist");
    let mut moving = Encoder::open(
        &directory.join("moving.mp4"),
        canvas::Canvas::new([1280, 720])?,
        FILE_LIMIT,
    )?;
    let mut image = frame(1280, 720, 240);
    for number in 0..120_u32 {
        for (index, pixel) in image.bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = u32::try_from(index % 1280)?;
            let y = u32::try_from(index / 1280)?;
            let ink = x.wrapping_add(number.wrapping_mul(11)) % 127 < 3
                || y.wrapping_add(number.wrapping_mul(7)) % 41 < 2;
            *pixel = if ink {
                [30, 30, 30, 255]
            } else {
                [240, 245, 250, 255]
            };
        }
        moving.write(
            &image,
            Duration::from_nanos(u64::from(number).saturating_mul(1_000_000_000) / 30),
        )?;
    }
    moving.finish(Some(Duration::from_secs(4)))?;
    let times = encoder::sample_times(&directory.join("moving.mp4"))?;
    assert_eq!(times.len(), 120);
    assert_eq!(times[0].0, 0);
    let &(last, duration) = times.last().unwrap();
    assert!((last + duration - 40_000_000).abs() <= 1_000);

    let mut held = Encoder::open(
        &directory.join("held.mp4"),
        canvas::Canvas::new([1280, 720])?,
        FILE_LIMIT,
    )?;
    held.write(&frame(1280, 720, 20), Duration::ZERO)?;
    held.write(&frame(1280, 720, 220), Duration::from_millis(1_500))?;
    held.finish(Some(Duration::from_secs(6)))?;
    let times = encoder::sample_times(&directory.join("held.mp4"))?;
    let &(last, duration) = times.last().unwrap();
    assert!(
        (last + duration - 60_000_000).abs() <= 1_000,
        "A quiet screen truncated the movie before recording ended"
    );

    let path = directory.join("capped.mp4");
    let limit = 1 << 20;
    let mut capped = Encoder::open(&path, canvas::Canvas::new([1280, 720])?, limit)?;
    let mut failed = false;
    for number in 0..900_u32 {
        for (index, pixel) in image.bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let shade = u8::try_from(
                (index.wrapping_add(usize::try_from(number)?.wrapping_mul(997))) % 251,
            )?;
            *pixel = [shade, shade, shade, 255];
        }
        if capped
            .write(
                &image,
                Duration::from_nanos(u64::from(number).saturating_mul(1_000_000_000) / 30),
            )
            .is_err()
        {
            failed = true;
            break;
        }
    }
    let finalized = capped.finish(None);
    assert!(
        failed || finalized.is_err(),
        "Native encoder did not exercise its byte cap"
    );
    assert!(std::fs::metadata(&path)?.len() <= limit);
    // Exclusive replacement verifies that synchronous teardown released the native file handle.
    std::fs::rename(&path, directory.join("capped-closed.mp4"))?;
    // Even initialization that cannot fit a header must release native references and close its
    // file; it cannot leave teardown waiting for work that can no longer be submitted.
    let too_small = directory.join("no-header.mp4");
    if let Ok(mut encoder) = Encoder::open(&too_small, canvas::Canvas::new([320, 240])?, 1) {
        let submitted = encoder.write(&frame(320, 240, 50), Duration::ZERO);
        let finished = encoder.finish(None);
        assert!(submitted.is_err() || finished.is_err());
    }
    assert!(std::fs::metadata(&too_small)?.len() <= 1);
    std::fs::remove_file(too_small)?;
    Ok(())
}
