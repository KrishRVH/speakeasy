//! The keeper owns real private files; producers use synthetic frames and PCM without devices.

use speakeasy_show::{Ending, Image, Moment, Take};

use super::*;

fn surface() -> Size {
    Size {
        width: 800.0,
        height: 600.0,
    }
}

fn begin(root: &Path) -> anyhow::Result<Kept> {
    let parent = root.to_owned();
    Keeper::start(move || Ok(parent), SystemTime::now())
}

fn record_frame(frames: &mut FrameLog) -> anyhow::Result<()> {
    assert!(frames.begin(1_000, surface()));
    let mut take = Take::new(1_000, surface());
    assert!(frames.send(take.arrive(Moment::START, || {
        Some(Image::from_bgra(2, 2, [1, 2, 3, 255].repeat(4)).unwrap())
    })?));
    assert!(frames.send(take.end(Moment::from_millis(1_000), Ending::Finished)));
    Ok(())
}

#[tokio::test]
async fn closing_frames_flushes_them_without_waiting_for_audio_or_releasing_the_lock()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let Kept {
        keeper,
        mut frames,
        mut audio,
    } = begin(root.path())?;
    record_frame(&mut frames)?;
    assert!(frames.close().await);
    assert_eq!(super::super::list(root.path(), SystemTime::now()).shows, []);
    let closing = keeper.close();
    tokio::pin!(closing);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut closing)
            .await
            .is_err()
    );
    assert!(audio.begin(8_000, Duration::from_millis(100), "en".into()));
    assert!(audio.finish(&[3, 0, 4, 0], &[(0, 0)]));
    let checkpoint = closing.await?;
    assert!(checkpoint.replay()?.ended());
    let kept = audio::read(File::open(checkpoint.directory.join(audio::FILE))?)?;
    assert_eq!(&*kept.pcm, &[3, 0, 4, 0]);
    assert!(kept.complete);
    checkpoint.discard()?;
    Ok(())
}

#[tokio::test]
async fn a_frame_failure_does_not_stop_the_audio_stream() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let Kept {
        keeper,
        mut frames,
        mut audio,
    } = begin(root.path())?;
    assert!(frames.begin(0, Size::default()));
    assert!(!frames.close().await);
    assert!(audio.begin(8_000, Duration::ZERO, "en".into()));
    assert!(audio.finish(&[5, 0], &[]));
    let checkpoint = keeper.close().await?;
    let kept = audio::read(File::open(checkpoint.directory.join(audio::FILE))?)?;
    assert_eq!(&*kept.pcm, &[5, 0]);
    checkpoint.discard()?;
    Ok(())
}

#[tokio::test]
async fn emergency_drop_stops_the_keeper_with_a_live_audio_producer() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let Kept {
        keeper,
        mut frames,
        mut audio,
    } = begin(root.path())?;
    record_frame(&mut frames)?;
    assert!(frames.close().await);
    tokio::time::timeout(
        crate::testing::PATIENCE,
        tokio::task::spawn_blocking(move || drop(keeper)),
    )
    .await??;
    assert!(!audio.begin(8_000, Duration::ZERO, "en".into()));
    let interrupted = super::super::list(root.path(), SystemTime::now())
        .shows
        .remove(0);
    assert!(interrupted.lock()?.replay()?.ended());
    Ok(())
}

#[tokio::test]
async fn an_audio_failure_does_not_stop_frames_or_release_the_owner() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let Kept {
        keeper,
        mut frames,
        mut audio,
    } = begin(root.path())?;
    assert!(audio.begin(0, Duration::ZERO, "en".into()));
    assert!(audio.finish(&[1, 0], &[]));
    record_frame(&mut frames)?;
    assert!(frames.close().await);
    let checkpoint = keeper.close().await?;
    assert!(checkpoint.replay()?.ended());
    assert!(checkpoint.audio().is_err());
    checkpoint.discard()?;
    Ok(())
}

#[tokio::test]
async fn failed_frame_and_closed_audio_lanes_cannot_unlock_a_native_movie() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let Kept {
        keeper,
        mut frames,
        audio,
    } = begin(root.path())?;
    record_frame(&mut frames)?;
    let target = frames.movie(Instant::now()).await.unwrap();
    // A duplicate stream header is a real write failure after the native lease was acquired.
    assert!(frames.begin(1_000, surface()));
    assert!(!frames.close().await);
    drop(audio);
    let closing = keeper.close();
    tokio::pin!(closing);
    let waiting = tokio::time::timeout(Duration::from_millis(20), &mut closing)
        .await
        .is_err();
    let cancelled = target.cancelled();
    let unlocked = super::super::list(root.path(), SystemTime::now()).shows;
    drop(target);
    let checkpoint = tokio::time::timeout(crate::testing::PATIENCE, closing).await??;
    assert!(waiting);
    assert!(!cancelled, "An unrelated frame failure stopped the movie");
    assert_eq!(unlocked, []);
    assert!(checkpoint.replay()?.ended());
    checkpoint.discard()?;
    Ok(())
}

#[tokio::test]
async fn emergency_drop_cancels_but_waits_for_the_native_movie_file_lease() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let Kept {
        keeper,
        mut frames,
        audio,
    } = begin(root.path())?;
    record_frame(&mut frames)?;
    let target = frames.movie(Instant::now()).await.unwrap();
    assert!(frames.close().await);
    drop(audio);
    let mut dropping = tokio::task::spawn_blocking(move || drop(keeper));
    tokio::time::timeout(crate::testing::PATIENCE, async {
        while !target.cancelled() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut dropping)
            .await
            .is_err()
    );
    assert_eq!(super::super::list(root.path(), SystemTime::now()).shows, []);
    drop(target);
    tokio::time::timeout(crate::testing::PATIENCE, dropping).await??;
    assert_eq!(
        super::super::list(root.path(), SystemTime::now())
            .shows
            .len(),
        1
    );
    Ok(())
}
