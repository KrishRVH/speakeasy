//! At most one recording device, open or retiring: the microphone, or a Show's screen recorder.
//! The owner opens only from `Free`, and retirement acknowledges native teardown before another
//! capture can start.

use std::{future::pending, mem};

use tokio::task::JoinHandle;

use super::{
    ports::{Capturing, Listening},
    reap,
};

#[derive(Default)]
pub(super) enum Device<R> {
    #[default]
    Free,
    Open(R),
    Retiring(JoinHandle<()>),
}

impl<R: Capturing> Device<R> {
    pub(super) const fn is_free(&self) -> bool {
        matches!(self, Self::Free)
    }

    pub(super) fn finish(&self) {
        if let Self::Open(recording) = self {
            recording.finish();
        }
    }

    pub(super) fn retire(&mut self) {
        *self = match mem::take(self) {
            Self::Open(recording) => Self::Retiring(tokio::spawn(recording.retire())),
            state @ (Self::Free | Self::Retiring(_)) => state,
        };
    }

    /// Pends unless retiring; resolves once the device is released, leaving it free. Cancel-safe:
    /// dropped early, it leaves the device retiring.
    pub(super) async fn retired(&mut self) {
        let Self::Retiring(task) = self else {
            return pending().await;
        };
        reap(task).await;
        *self = Self::Free;
    }

    /// Resolves once any open or retiring device has been released.
    pub(super) async fn stop(mut self) {
        self.retire();
        if let Self::Retiring(task) = self {
            reap(task).await;
        }
    }
}

impl<R: Listening> Device<R> {
    pub(super) fn expect_cue(&self) {
        if let Self::Open(recording) = self {
            recording.expect_cue();
        }
    }
}
