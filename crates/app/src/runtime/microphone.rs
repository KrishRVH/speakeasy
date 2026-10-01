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
    pub fn free(&self) -> bool {
        matches!(self, Self::Free)
    }
    pub fn recording(&self) -> Option<&R> {
        match self {
            Self::Open(recording) => Some(recording),
            _ => None,
        }
    }
    pub fn retire(&mut self) {
        *self = match std::mem::replace(self, Self::Free) {
            Self::Open(recording) => Self::Retiring(tokio::spawn(recording.retire())),
            state => state,
        };
    }
    pub async fn retired(&mut self) {
        match self {
            Self::Retiring(task) => {
                let _ = task.await;
            }
            _ => std::future::pending().await,
        }
    }
    pub async fn stop(mut self) {
        self.retire();
        if let Self::Retiring(task) = self {
            let _ = task.await;
        }
    }
}
