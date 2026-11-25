use std::path::{Path, PathBuf};

use super::*;
use crate::{
    MovieControl, MovieEvent,
    movie::{encode::Clock, tests::frame},
};

#[cfg(not(target_os = "windows"))]
const NATIVE_CLOCK: Clock = Clock::Nanoseconds;
#[cfg(target_os = "windows")]
const NATIVE_CLOCK: Clock = Clock::HundredNanoseconds;

struct Accepting;

impl Codec for Accepting {
    const CLOCK: Clock = NATIVE_CLOCK;

    fn open(_: &Path, _: [u32; 2], _: u64) -> anyhow::Result<Self> {
        Ok(Self)
    }

    fn write(
        &mut self,
        _: &[u8],
        _: u32,
        _: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<ControlFlow<()>> {
        Ok(ControlFlow::Continue(()))
    }

    fn finish(self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Refusing;

impl Codec for Refusing {
    const CLOCK: Clock = NATIVE_CLOCK;

    fn open(_: &Path, _: [u32; 2], _: u64) -> anyhow::Result<Self> {
        Ok(Self)
    }

    fn write(
        &mut self,
        _: &[u8],
        _: u32,
        _: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<ControlFlow<()>> {
        anyhow::bail!("The synthetic writer refused a frame")
    }

    fn finish(self) -> anyhow::Result<()> {
        panic!("A failed movie was finalized")
    }
}

fn control() -> (MovieControl, MovieTarget) {
    MovieControl::new(
        PathBuf::from("synthetic.mp4"),
        Instant::now()
            .checked_add(INTERVAL.saturating_mul(150))
            .unwrap(),
        Duration::MAX,
    )
}

async fn accept_first_frame(control: &MovieControl) -> anyhow::Result<()> {
    let Some(MovieEvent::Started { accepted, .. }) = control.next().await else {
        anyhow::bail!("Synthetic movie didn't request its origin");
    };
    assert!(accepted.send(true).is_ok());
    let Some(MovieEvent::Geometry {
        sample: 0,
        accepted,
        ..
    }) = control.next().await
    else {
        anyhow::bail!("Synthetic movie didn't request its first content rectangle");
    };
    assert!(accepted.send(true).is_ok());
    Ok(())
}

async fn record<C: Codec>(stopped: bool) -> anyhow::Result<(bool, Option<(u32, bool)>)> {
    let (control, target) = control();
    let (written, wrote) = async_channel::bounded(1);
    let native = crate::OwnedThread::spawn("synthetic movie", move || {
        let (_stop, stopping) = async_channel::bounded(1);
        let mut movie = Movie::<C>::Pending(target);
        let refused = movie.frame(&frame(2, 2, 140), &stopping).is_err();
        if stopped {
            movie.stop(Instant::now());
        }
        assert!(!movie.wants_frames());
        written.send_blocking(refused).unwrap();
    })?;
    accept_first_frame(&control).await?;
    native.join().await?;
    let refused = wrote.recv().await?;
    let finished = match control.next().await {
        Some(MovieEvent::Finished {
            frames, finalized, ..
        }) => Some((frames, finalized)),
        _ => None,
    };
    assert!(control.next().await.is_none());
    Ok((refused, finished))
}

#[tokio::test]
async fn a_stopped_movie_is_finalized() -> anyhow::Result<()> {
    assert_eq!(record::<Accepting>(true).await?, (false, Some((1, true))));
    Ok(())
}

#[tokio::test]
async fn a_failed_movie_is_discarded_without_finalizing() -> anyhow::Result<()> {
    assert_eq!(record::<Refusing>(false).await?, (true, Some((0, false))));
    Ok(())
}

#[tokio::test]
async fn a_quiet_screen_covers_no_more_than_the_recording_limit() -> anyhow::Result<()> {
    const LIMIT: Duration = Duration::from_secs(1);
    const POLL: Duration = Duration::from_millis(250);
    let first = frame(2, 2, 140);
    let start = first.at;
    let (control, target) = MovieControl::new(
        PathBuf::from("synthetic.mp4"),
        Instant::now()
            .checked_add(INTERVAL.saturating_mul(150))
            .unwrap(),
        LIMIT,
    );
    let native = crate::OwnedThread::spawn("synthetic quiet movie", move || {
        let (_stop, stopping) = async_channel::bounded(1);
        let mut movie = Movie::<Accepting>::Pending(target);
        assert!(movie.frame(&first, &stopping).is_ok());
        for step in 1..=8 {
            let now = start.checked_add(POLL.saturating_mul(step)).unwrap();
            assert!(movie.poll(now, &stopping).is_ok());
        }
        movie.stop(start.checked_add(LIMIT.saturating_mul(2)).unwrap());
    })?;
    accept_first_frame(&control).await?;
    native.join().await?;
    let Some(MovieEvent::Finished {
        through, frames, ..
    }) = control.next().await
    else {
        anyhow::bail!("Synthetic movie didn't finish");
    };
    assert_eq!((frames, through), (31, start.checked_add(LIMIT).unwrap()));
    Ok(())
}

#[tokio::test]
async fn a_cancelled_pending_movie_releases_its_lease_without_a_frame() {
    let (control, target) = control();
    let (_stop, stopping) = async_channel::bounded(1);
    let mut movie = Movie::<Accepting>::Pending(target);
    control.request_stop();
    assert!(movie.poll(Instant::now(), &stopping).is_ok());
    assert!(!movie.wants_frames());
    assert!(control.next().await.is_none());
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[tokio::test]
#[ignore = "Opt in: native movie encoder with synthetic pixels only"]
async fn a_cancelled_active_movie_releases_its_file_without_a_new_frame() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("synthetic.mp4");
    let (control, target) = MovieControl::new(
        path.clone(),
        Instant::now()
            .checked_add(INTERVAL.saturating_mul(150))
            .unwrap(),
        Duration::MAX,
    );
    let (_stop, stopping) = async_channel::bounded(1);
    let (submitted, wrote) = async_channel::bounded(1);
    let (poll, polling) = async_channel::bounded(1);
    let native = crate::OwnedThread::spawn("synthetic movie cancellation", move || {
        let mut movie = Movie::new(Some(target));
        assert!(movie.frame(&frame(320, 240, 140), &stopping).is_ok());
        submitted.send_blocking(()).unwrap();
        if polling.recv_blocking().is_ok() {
            assert!(movie.poll(Instant::now(), &stopping).is_ok());
            assert!(!movie.wants_frames());
        }
    })?;
    let Some(crate::MovieEvent::Started { accepted, .. }) = control.next().await else {
        anyhow::bail!("Synthetic movie didn't request its origin");
    };
    assert!(accepted.send(true).is_ok());
    let Some(crate::MovieEvent::Geometry {
        sample, accepted, ..
    }) = control.next().await
    else {
        anyhow::bail!("Synthetic movie didn't request its content rectangle");
    };
    assert_eq!(sample, 0);
    assert!(accepted.send(true).is_ok());
    wrote.recv().await?;
    control.request_stop();
    poll.send(()).await?;
    native.join().await?;
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
