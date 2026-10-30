//! Accepted library writes belong to the app, independently of the window showing their results.
//! One short-lived thread serializes writes; closing a window drops only its result receiver.

use std::{path::PathBuf, sync::Arc};

use async_channel::Receiver;
use speakeasy_platform::OwnedThread;

use super::{
    Interrupted,
    store::retention::{self, Retention},
};

pub(crate) enum Edit {
    Keep {
        parent: PathBuf,
        directory: PathBuf,
        kept: bool,
    },
    Discard(Interrupted),
}

pub(crate) type Receipt = Receiver<anyhow::Result<Option<Retention>>>;

#[derive(Default)]
pub(crate) struct Edits {
    writing: Option<OwnedThread>,
}

impl Edits {
    pub(crate) fn submit(
        &mut self,
        edit: Edit,
        fixture: Option<Arc<tempfile::TempDir>>,
    ) -> anyhow::Result<Receipt> {
        self.start(move || {
            // The immutable lease keeps a demo's private folder alive until its write finishes.
            let _fixture = fixture;
            match edit {
                Edit::Keep {
                    parent,
                    directory,
                    kept,
                } => retention::set_kept(&parent, &directory, kept)
                    .map(Some)
                    .map_err(Into::into),
                Edit::Discard(interrupted) => interrupted.lock()?.discard().map(|()| None),
            }
        })
    }

    fn start(
        &mut self,
        write: impl FnOnce() -> anyhow::Result<Option<Retention>> + Send + 'static,
    ) -> anyhow::Result<Receipt> {
        anyhow::ensure!(
            self.writing.as_ref().is_none_or(OwnedThread::is_finished),
            "A Show change is still being saved. Try again when it finishes."
        );
        let (done, result) = async_channel::bounded(1);
        self.writing = Some(OwnedThread::spawn("show-edit", move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Closing the view drops its receipt, but the accepted write still runs"
            )]
            let _ = done.try_send(write());
        })?);
        Ok(result)
    }

    /// Quit awaits the body while retaining this owner; Drop joins even during forced teardown.
    pub(crate) fn drained(&self) -> impl Future<Output = ()> + use<> {
        let writing = self.writing.as_ref().map(OwnedThread::exited);
        async move {
            if let Some(writing) = writing {
                writing.await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closing_the_view_keeps_accepted_writes_and_quit_waits_for_them() -> anyhow::Result<()>
    {
        let mut edits = Edits::default();
        let (release, wait) = std::sync::mpsc::channel();
        let (wrote, written) = async_channel::bounded(1);
        let result = edits.start(move || {
            wait.recv()?;
            wrote.try_send(())?;
            Ok(None)
        })?;
        drop(result);
        assert!(edits.start(|| Ok(None)).is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), edits.drained())
                .await
                .is_err()
        );
        release.send(())?;
        edits.drained().await;
        written.try_recv()?;
        assert!(edits.start(|| Ok(None)).is_ok());
        Ok(())
    }
}
