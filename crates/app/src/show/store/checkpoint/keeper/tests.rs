use std::time::Duration;

use speakeasy_show::{Ending, Image, Moment, Take};

use super::*;
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
async fn closing_waits_for_frames_and_audio_and_keeps_both() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let Kept {
        keeper,
        mut frames,
        mut audio,
    } = begin(root.path())?;
    record_frame(&mut frames)?;
    drop(frames);
    let closing = keeper.close();
    tokio::pin!(closing);
    assert!(still_pending(&mut closing).await);
    assert!(audio.begin(8_000, Duration::from_millis(100), "en".into()));
    audio.finish(&[3, 0, 4, 0], &[(0, 0)], AudioEnding::Complete);
    let checkpoint = closing.await?.context("Fixture checkpoint is missing")?;
    assert!(checkpoint.replay()?.ended());
    let kept = checkpoint.audio()?.context("Fixture audio is missing")?;
    assert_eq!(kept.pcm, [3, 0, 4, 0]);
    assert!(kept.complete);
    checkpoint.discard()
}

#[tokio::test]
async fn discarding_stops_the_movie_and_waits_for_its_native_lease() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let Kept {
        keeper,
        mut frames,
        audio,
    } = begin(root.path())?;
    record_frame(&mut frames)?;
    let target = frames
        .movie(Instant::now())
        .await
        .context("The movie target is missing")?;
    drop(frames);
    drop(audio);
    keeper.discard();
    tokio::time::timeout(PATIENCE, async {
        while !target.cancelled() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let closing = keeper.close();
    tokio::pin!(closing);
    assert!(still_pending(&mut closing).await);
    drop(target);
    let checkpoint = tokio::time::timeout(PATIENCE, closing)
        .await??
        .context("Fixture checkpoint is missing")?;
    checkpoint.discard()
}
