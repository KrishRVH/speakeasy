//! A checkpoint's independent movie-write lease. Cancellation asks the native writer to stop;
//! closing the event lane acknowledges that its encoder and file handles have been released.

use std::{
    convert::Infallible,
    path::{Path, PathBuf},
    time::Instant,
};

use async_channel::{Receiver, Sender};

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
    /// recovery and deletion of any partial file.
    #[must_use]
    pub fn new(path: PathBuf) -> (Self, MovieTarget) {
        let (cancel, cancelled) = async_channel::bounded(1);
        let (outgoing, events) = async_channel::bounded(2);
        (
            Self { cancel, events },
            MovieTarget {
                path,
                cancelled,
                events: outgoing,
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
        self.cancelled.is_closed() || self.events.is_closed()
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn begin(
        &self,
        at: Instant,
        size: [u32; 2],
        stopping: &Receiver<Infallible>,
    ) -> bool {
        let (accepted, answer) = async_channel::bounded(1);
        if self
            .events
            .try_send(MovieEvent::Started { at, size, accepted })
            .is_err()
        {
            return false;
        }
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
            return false;
        };
        runtime.block_on(async {
            tokio::select! {
                biased;
                _cancelled = self.cancelled.recv() => false,
                _stopped = stopping.recv() => false,
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
    async fn cancellation_does_not_acknowledge_a_native_writer_still_holding_its_lease() {
        let (control, target) = MovieControl::new(PathBuf::from("synthetic.mp4"));
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
            let (control, target) = MovieControl::new(PathBuf::from("synthetic.mp4"));
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
