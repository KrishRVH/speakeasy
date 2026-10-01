//! At most one device, open or retiring. Opening is permitted only from Free;
//! retirement acknowledges native teardown before another capture can start.
use super::Recording;
use tokio::task::JoinHandle;
#[derive(Default)]
pub(super) enum Microphone<R> {
    #[default]
    Free,
    Open(R),
    Retiring(JoinHandle<()>),
}
impl<R: Recording> Microphone<R> {
    pub(super) fn free(&self) -> bool {
        matches!(self, Self::Free)
    }
    pub(super) fn recording(&self) -> Option<&R> {
        match self {
            Self::Open(recording) => Some(recording),
            _ => None,
        }
    }
    pub(super) fn retire(&mut self) {
        *self = match std::mem::replace(self, Self::Free) {
            Self::Open(recording) => Self::Retiring(tokio::spawn(recording.retire())),
            state => state,
        };
    }
    pub(super) async fn retired(&mut self) {
        match self {
            Self::Retiring(task) => {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A completed or panicked retirement task has released its capture; neither may keep the device occupied"
                )]
                let _ = task.await;
            },
            _ => std::future::pending().await,
        }
    }
    pub(super) async fn stop(mut self) {
        self.retire();
        if let Self::Retiring(task) = self {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Shutdown reaps the retirement task even after a panic; there is no remaining session to update"
            )]
            let _ = task.await;
        }
    }
}
