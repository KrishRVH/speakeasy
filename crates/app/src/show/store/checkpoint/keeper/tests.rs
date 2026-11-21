use speakeasy_show::{Ending, Image, Moment, Take};

use super::{super::list, *};
use crate::testing::{PATIENCE, still_pending};

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
    assert!(frames.begin(1_000, surface(), speakeasy_show::Framing::Automatic));
    let mut take = Take::new(1_000, surface(), speakeasy_show::Framing::Automatic);
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
    assert_eq!(list(root.path(), SystemTime::now()).shows, []);
    let closing = keeper.close();
    tokio::pin!(closing);
    assert!(still_pending(&mut closing).await);
    assert!(audio.begin(8_000, Duration::from_millis(100), "en".into()));
    assert!(audio.finish(&[3, 0, 4, 0], &[(0, 0)], AudioEnding::Complete));
    let checkpoint = closing.await?.context("Fixture checkpoint is missing")?;
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
    assert!(frames.begin(0, Size::default(), speakeasy_show::Framing::Automatic));
    assert!(!frames.close().await);
    assert!(audio.begin(8_000, Duration::ZERO, "en".into()));
    assert!(audio.finish(&[5, 0], &[], AudioEnding::Complete));
    let checkpoint = keeper
        .close()
        .await?
        .context("Fixture checkpoint is missing")?;
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
    tokio::time::timeout(PATIENCE, tokio::task::spawn_blocking(move || drop(keeper))).await??;
    assert!(!audio.begin(8_000, Duration::ZERO, "en".into()));
    let interrupted = list(root.path(), SystemTime::now()).shows.remove(0);
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
    drop(audio);
    record_frame(&mut frames)?;
    assert!(frames.close().await);
    let checkpoint = keeper
        .close()
        .await?
        .context("Fixture checkpoint is missing")?;
    assert!(checkpoint.replay()?.ended());
    assert!(checkpoint.audio()?.is_none());
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

    assert!(frames.begin(1_000, surface(), speakeasy_show::Framing::Automatic));
    assert!(!frames.close().await);
    drop(audio);
    let closing = keeper.close();
    tokio::pin!(closing);
    let waiting = still_pending(&mut closing).await;
    let cancelled = target.cancelled();
    let unlocked = list(root.path(), SystemTime::now()).shows;
    drop(target);
    let checkpoint = tokio::time::timeout(PATIENCE, closing)
        .await??
        .context("Fixture checkpoint is missing")?;
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
    tokio::time::timeout(PATIENCE, async {
        while !target.cancelled() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(still_pending(&mut dropping).await);
    assert_eq!(list(root.path(), SystemTime::now()).shows, []);
    drop(target);
    tokio::time::timeout(PATIENCE, dropping).await??;
    assert_eq!(list(root.path(), SystemTime::now()).shows.len(), 1);
    Ok(())
}

#[tokio::test]
async fn discarding_records_its_intent_before_the_native_movie_lease_closes() -> anyhow::Result<()>
{
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
    keeper.discard();
    tokio::time::timeout(PATIENCE, async {
        while !target.cancelled() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let marked = std::fs::read_dir(root.path().join(super::super::ROOT))?
        .filter_map(Result::ok)
        .any(|entry| entry.path().join(super::super::DISCARDED).is_file());
    drop(target);
    let checkpoint = tokio::time::timeout(PATIENCE, keeper.close())
        .await??
        .context("Fixture checkpoint is missing")?;
    assert!(
        marked,
        "Discard waited for the movie lease to record itself"
    );
    drop(checkpoint);
    assert_eq!(list(root.path(), SystemTime::now()).shows, []);
    Ok(())
}

#[tokio::test]
async fn a_lost_completion_stays_an_error_after_checkpoint_files_are_written() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let Kept {
        keeper,
        mut frames,
        audio,
    } = begin(root.path())?;
    record_frame(&mut frames)?;
    assert!(frames.close().await);
    drop(audio);
    let checkpoint = tokio::time::timeout(PATIENCE, keeper.closed.recv())
        .await???
        .context("Fixture checkpoint is missing")?;
    let interrupted = checkpoint.interrupted()?;
    let error = keeper
        .close()
        .await
        .err()
        .context("A lost receipt was ignored")?;
    assert_eq!(
        error.to_string(),
        "Show checkpoint stopped before returning its files"
    );
    let recovered = interrupted.lock()?;
    assert!(recovered.replay()?.ended());
    recovered.discard()?;
    Ok(())
}
