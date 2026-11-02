#[cfg(any(test, target_os = "windows", target_os = "macos"))]
pub(crate) mod canvas;
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) mod encode;
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) mod recording;

use std::{
    convert::Infallible,
    path::{Path, PathBuf},
    time::Instant,
};

use async_channel::{Receiver, Sender};
use speakeasy_show::MoviePrefix;

pub struct MovieFrame {
    pub at: std::time::Duration,
    pub size: [u32; 2],
    pub bgra: Vec<u8>,
}

impl std::fmt::Debug for MovieFrame {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("MovieFrame")
            .field("at", &self.at)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

pub fn read_movie_frame(
    file: std::fs::File,
    prefix: &MoviePrefix,
    at: std::time::Duration,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
    anyhow::ensure!(
        file.metadata()?.len() == prefix.bytes
            && (1..=MoviePrefix::FILE_LIMIT).contains(&prefix.bytes),
        "Movie changed; refresh the Show before reading it"
    );
    anyhow::ensure!(
        prefix
            .size
            .into_iter()
            .all(|edge| (2..=2560).contains(&edge) && edge.is_multiple_of(2))
            && at <= prefix.duration
            && at <= std::time::Duration::from_secs(300),
        "Movie frame request is out of range"
    );
    if cancelled() {
        return Ok(None);
    }
    #[cfg(target_os = "windows")]
    {
        crate::windows::read_movie_frame(file, prefix.bytes, prefix.size, at, &cancelled)
    }
    #[cfg(target_os = "macos")]
    {
        use anyhow::Context as _;
        let index = prefix
            .samples()
            .partition_point(|sample| sample.at <= at)
            .saturating_sub(1);
        let sample = prefix
            .samples()
            .get(index)
            .context("Movie has no retained sample")?;
        anyhow::ensure!(
            (1..=1_000_000_000).contains(&prefix.timescale)
                && sample.ticks <= u64::from(prefix.timescale).saturating_mul(360),
            "Movie sample timestamp is out of range"
        );
        crate::macos::movie::frame(file, prefix, (sample.ticks, prefix.timescale), &cancelled)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        anyhow::bail!(
            "Video frame decoding is available on Windows and macOS. Open recording.mp4 in your system player."
        )
    }
}

#[derive(Debug)]
pub enum MovieEvent {
    Started {
        at: Instant,
        size: [u32; 2],
        accepted: Sender<bool>,
    },
    Geometry {
        sample: u32,
        content: [u32; 4],
        part: crate::Part,
        accepted: Sender<bool>,
    },
    Finished {
        last: Instant,
        through: Instant,
        frames: u32,
        finalized: bool,
    },
}

#[derive(Debug)]
pub struct MovieControl {
    cancel: Sender<Infallible>,
    events: Receiver<MovieEvent>,
}

pub struct MovieTarget {
    path: PathBuf,
    cancelled: Receiver<Infallible>,
    events: Sender<MovieEvent>,
    deadline: Instant,
}

impl std::fmt::Debug for MovieTarget {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("MovieTarget").finish_non_exhaustive()
    }
}

impl MovieControl {
    #[must_use]
    pub fn new(path: PathBuf, deadline: Instant) -> (Self, MovieTarget) {
        let (cancel, cancelled) = async_channel::bounded(1);
        let (outgoing, events) = async_channel::bounded(2);
        (
            Self { cancel, events },
            MovieTarget {
                path,
                cancelled,
                events: outgoing,
                deadline,
            },
        )
    }

    pub fn request_stop(&self) {
        self.cancel.close();
    }

    pub async fn next(&self) -> Option<MovieEvent> {
        self.events.recv().await.ok()
    }

    pub fn close_blocking(&self) {
        self.request_stop();
        while self.events.recv_blocking().is_ok() {}
    }
}

impl Drop for MovieControl {
    fn drop(&mut self) {
        self.request_stop();
    }
}

impl MovieTarget {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.cancelled.is_closed() || self.events.is_closed() || Instant::now() >= self.deadline
    }

    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn begin(
        &self,
        at: Instant,
        size: [u32; 2],
        stopping: &Receiver<Infallible>,
    ) -> bool {
        if at >= self.deadline || self.cancelled() {
            return false;
        }
        let (accepted, answer) = async_channel::bounded(1);
        if self
            .events
            .try_send(MovieEvent::Started { at, size, accepted })
            .is_err()
        {
            return false;
        }
        self.accepted(answer, stopping).unwrap_or(false)
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn geometry(
        &self,
        sample: u32,
        content: [u32; 4],
        part: crate::Part,
        stopping: &Receiver<Infallible>,
    ) -> anyhow::Result<bool> {
        if self.cancelled() || stopping.is_closed() {
            return Ok(false);
        }
        let (accepted, answer) = async_channel::bounded(1);
        self.events
            .try_send(MovieEvent::Geometry {
                sample,
                content,
                part,
                accepted,
            })
            .map_err(|_closed| anyhow::anyhow!("Video crop geometry could not be saved"))?;
        self.accepted(answer, stopping)
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    fn accepted(
        &self,
        answer: Receiver<bool>,
        stopping: &Receiver<Infallible>,
    ) -> anyhow::Result<bool> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let accepted = runtime.block_on(async {
            tokio::select! {
                biased;
                _cancelled = self.cancelled.recv() => None,
                _stopped = stopping.recv() => None,
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline)) => None,
                accepted = answer.recv() => Some(accepted.unwrap_or(false)),
            }
        });
        if self.cancelled() || stopping.is_closed() || accepted.is_none() {
            return Ok(false);
        }
        anyhow::ensure!(
            accepted == Some(true),
            "Video crop geometry could not be saved"
        );
        Ok(true)
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn finish(&self, last: Instant, through: Instant, frames: u32, finalized: bool) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A cancelled checkpoint may stop observing; the lease still closes only after native teardown"
        )]
        let _ = self.events.try_send(MovieEvent::Finished {
            last,
            through,
            frames,
            finalized,
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        future::Future as _,
        task::{Context, Waker},
    };

    use super::*;

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn color_frame(index: usize, size: [u32; 2]) -> crate::Frame {
        assert!(matches!(size, [320, 240] | [1280, 720] | [2560, 1440]));
        let [width, height] = size.map(|edge| usize::try_from(edge).unwrap());
        let mut bgra =
            Vec::with_capacity(width.checked_mul(height).unwrap().checked_mul(4).unwrap());
        let blue = if index.is_multiple_of(2) { 20 } else { 230 };
        for row in 0..height {
            for column in 0..width {
                let (x, y) = (
                    column.checked_mul(320).unwrap().checked_div(width).unwrap(),
                    row.checked_mul(240).unwrap().checked_div(height).unwrap(),
                );
                let color = if (100..220).contains(&x) && (80..160).contains(&y) {
                    [blue, 80, 150]
                } else {
                    match (x >= 160, y >= 120) {
                        (false, false) => [20, 30, 230],
                        (true, false) => [30, 220, 20],
                        (false, true) => [220, 20, 30],
                        (true, true) => [180, 180, 180],
                    }
                };
                let color = match (size, x, y) {
                    ([1280, 720], 180..220, 180..220) => [64; 3],
                    ([1280, 720], 230..270, 180..220) => [128; 3],
                    _ => color,
                };
                bgra.extend(color);
                bgra.push(255);
            }
        }
        crate::Frame {
            at: Instant::now(),
            width: size[0],
            height: size[1],
            bgra,
            part: crate::Part::WHOLE,
        }
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn check_colors(frame: &MovieFrame, index: usize) {
        assert!(matches!(
            frame.size,
            [320, 240] | [1280, 720] | [2560, 1440]
        ));
        let [width, height] = frame.size.map(|edge| usize::try_from(edge).unwrap());
        let (pixels, remainder) = frame.bgra.as_chunks::<4>();
        assert_eq!(remainder, b"");
        assert_eq!(pixels.len(), width.checked_mul(height).unwrap());
        assert!(pixels.iter().all(|pixel| pixel[3] == 255));
        for (x, y, expected) in [
            (20, 20, [20, 30, 230]),
            (300, 20, [30, 220, 20]),
            (20, 220, [220, 20, 30]),
            (300, 220, [180, 180, 180]),
            (
                160,
                120,
                [if index.is_multiple_of(2) { 20 } else { 230 }, 80, 150],
            ),
        ] {
            let row = height.checked_mul(y).unwrap() / 240;
            let column = width.checked_mul(x).unwrap() / 320;
            let offset = row.checked_mul(width).unwrap().checked_add(column).unwrap();
            assert!(
                pixels[offset][..3]
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual.abs_diff(expected) < 12),
                "Synthetic pixel at ({x}, {y}) changed color, orientation, or sample"
            );
        }
        if frame.size == [1280, 720] {
            for (x, shade) in [(200, 64_u8), (250, 128)] {
                let row = height.checked_mul(200).unwrap() / 240;
                let column = width.checked_mul(x).unwrap() / 320;
                let offset = row.checked_mul(width).unwrap().checked_add(column).unwrap();
                assert!(
                    pixels[offset][..3]
                        .iter()
                        .all(|value| value.abs_diff(shade) < 12),
                    "HD neutral midtone changed its transfer or range"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_session_deadline_cancels_an_unused_target_without_releasing_its_lease() {
        let (control, target) = MovieControl::new(PathBuf::from("synthetic.mp4"), Instant::now());
        assert!(target.cancelled());
        let mut waiting = std::pin::pin!(control.next());
        assert!(
            waiting
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        drop(target);
        assert!(waiting.await.is_none());
    }

    #[tokio::test]
    async fn cancellation_does_not_acknowledge_a_native_writer_still_holding_its_lease() {
        let (control, target) = MovieControl::new(
            PathBuf::from("synthetic.mp4"),
            Instant::now().checked_add(crate::PATIENCE).unwrap(),
        );
        control.request_stop();
        assert!(target.cancelled());
        let mut waiting = std::pin::pin!(control.next());
        assert!(
            waiting
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        drop(target);
        assert!(waiting.await.is_none());
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn checkpoint_and_capture_cancellation_interrupt_an_unanswered_acceptance()
    -> anyhow::Result<()> {
        for cancel_checkpoint in [true, false] {
            let (control, target) = MovieControl::new(
                PathBuf::from("synthetic.mp4"),
                Instant::now().checked_add(crate::PATIENCE).unwrap(),
            );
            let (stop, stopping) = async_channel::bounded(1);
            let native = crate::OwnedThread::spawn("synthetic movie handshake", move || {
                assert!(!target.begin(Instant::now(), [2, 2], &stopping));
            })?;
            let Some(MovieEvent::Started { accepted, .. }) = control.next().await else {
                anyhow::bail!("Synthetic movie did not ask for acceptance");
            };
            if cancel_checkpoint {
                control.request_stop();
            } else {
                stop.close();
            }
            let exited = tokio::time::timeout(crate::PATIENCE, native.exited()).await;

            drop(accepted);
            native.join().await?;
            assert!(
                exited.is_ok(),
                "Cancellation waited for checkpoint approval"
            );
            assert!(control.next().await.is_none());
        }
        Ok(())
    }
}
