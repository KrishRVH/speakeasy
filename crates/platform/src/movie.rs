//! A checkpoint's independent movie-write lease. Cancellation asks the native writer to stop;
//! closing the event lane acknowledges that its encoder and file handles have been released.

use std::{
    convert::Infallible,
    path::{Path, PathBuf},
    time::Instant,
};

use async_channel::{Receiver, Sender};

/// A decoded video sample. Pixel rows are top-down, tightly packed opaque BGRA.
pub struct MovieFrame {
    /// Actual presentation time relative to the movie's zero origin.
    pub at: std::time::Duration,
    /// Encoded canvas dimensions.
    pub size: [u32; 2],
    /// The sample's private pixels; never included in diagnostics.
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

/// Reads the video sample at or immediately before `at`, or the first sample if seeking lands
/// later. The returned timestamp is authoritative.
///
/// The caller must first validate the file's
/// silent single-track container and canvas, using the same open file handle.
///
/// Runs blocking native work on the calling background thread. Cancellation is checked between
/// native calls; a blocked codec must finish before this returns and releases all its resources.
/// Returns `None` on cancellation. No decoder or file remains open after return.
///
/// # Errors
/// Rejects changed file length, canvas mismatch, invalid samples, and native decoder failure.
/// Currently supported on Windows; other hosts report unavailability without opening a decoder.
pub fn read_movie_frame(
    file: std::fs::File,
    bytes: u64,
    size: [u32; 2],
    at: std::time::Duration,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
    anyhow::ensure!(
        file.metadata()?.len() == bytes && (1..=1 << 30).contains(&bytes),
        "Movie changed; refresh the Show before reading it"
    );
    anyhow::ensure!(
        size.into_iter()
            .all(|edge| (2..=2560).contains(&edge) && edge.is_multiple_of(2))
            && at <= std::time::Duration::from_secs(300),
        "Movie frame request is out of range"
    );
    if cancelled() {
        return Ok(None);
    }
    #[cfg(target_os = "windows")]
    {
        crate::windows::read_movie_frame(file, size, at, &cancelled)
    }
    #[cfg(not(target_os = "windows"))]
    {
        anyhow::bail!(
            "Video frame decoding is currently available on Windows. Open recording.mp4 in your system player."
        )
    }
}

/// Native movie events. A checkpoint must retain its lock until this lane closes, even after an
/// error or cancellation. No event contains pixels or narration.
#[derive(Debug)]
pub enum MovieEvent {
    /// The first admitted frame, before the movie file is created. Accept only after the origin
    /// and canvas have been saved durably; MP4 normalizes its first timestamp to zero.
    Started {
        /// Capture timestamp on the same clock as screen frames.
        at: Instant,
        /// Fixed encoded canvas, in pixels.
        size: [u32; 2],
        /// `true` permits encoding; closing or rejecting prevents creation of the movie.
        accepted: Sender<bool>,
    },
    /// Submitted frame coverage, after encoder teardown. An unfinished file must be inspected
    /// for a playable fragment prefix before claiming that coverage survived.
    Finished {
        /// Timestamp of the last submitted frame.
        last: Instant,
        /// Capture's observed end. Playback may hold its final image through this time; it does
        /// not imply that another image was captured then.
        through: Instant,
        /// Number of submitted frames.
        frames: u32,
        /// Whether native finalization succeeded.
        finalized: bool,
    },
}

/// The checkpoint's half of a movie-write lease. Dropping it requests cancellation but does not
/// acknowledge native teardown; owners must wait for `next` to return `None` before unlocking.
#[derive(Debug)]
pub struct MovieControl {
    cancel: Sender<Infallible>,
    events: Receiver<MovieEvent>,
}

/// The native capture thread's non-cloneable movie-write lease. It must be dropped only after all
/// native objects and file handles for this movie are released, never on an outer capture handle.
pub struct MovieTarget {
    pub(crate) path: PathBuf,
    cancelled: Receiver<Infallible>,
    events: Sender<MovieEvent>,
    deadline: Instant,
}

/// Leaves the private checkpoint path out of diagnostics.
impl std::fmt::Debug for MovieTarget {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("MovieTarget").finish_non_exhaustive()
    }
}

impl MovieControl {
    /// Reserves a new movie path beneath a private, locked checkpoint. This opens no file. The
    /// native writer refuses an existing destination; the checkpoint remains responsible for
    /// recovery and deletion of any partial file. `deadline` is the original session's hard end,
    /// even when movie capture begins later.
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

    /// Requests cancellation without waiting for a potentially blocked native codec call.
    pub fn request_stop(&self) {
        self.cancel.close();
    }

    /// Receives the next event, or `None` only after the native lease has been released.
    pub async fn next(&self) -> Option<MovieEvent> {
        self.events.recv().await.ok()
    }

    /// Cancels and drains native teardown on an owning background thread. This also drops any
    /// unanswered acceptance request, so an unwinding checkpoint keeps its lock until every
    /// native file handle has released. Never call this on a UI thread or while holding the target.
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
    /// The unused path reserved beneath the locked checkpoint.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the checkpoint has requested that movie writing stop.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.cancelled.is_closed() || self.events.is_closed() || Instant::now() >= self.deadline
    }

    /// The original session's hard recording deadline. It bounds input and held-tail timing;
    /// reaching it never permits the checkpoint to unlock before native cleanup acknowledges.
    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }

    #[cfg(target_os = "windows")]
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
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        else {
            return false;
        };
        runtime.block_on(async {
            tokio::select! {
                biased;
                _cancelled = self.cancelled.recv() => false,
                _stopped = stopping.recv() => false,
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline)) => false,
                accepted = answer.recv() => accepted.unwrap_or(false),
            }
        }) && !self.cancelled()
            && !stopping.is_closed()
    }

    #[cfg(target_os = "windows")]
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
mod tests {
    use std::{
        future::Future as _,
        task::{Context, Waker},
    };

    use super::*;

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
            // On failure, release the waiter before joining so the test cannot deadlock on unwind.
            drop(accepted);
            native.join().await;
            assert!(
                exited.is_ok(),
                "Cancellation waited for checkpoint approval"
            );
            assert!(control.next().await.is_none());
        }
        Ok(())
    }
}
