use std::sync::Arc;

use async_channel::Receiver;
use speakeasy_platform::OwnedThread;

pub(crate) struct Edit(Box<dyn FnOnce() + Send>);

impl Edit {
    pub(crate) fn new<T: Send + 'static>(
        write: impl FnOnce() -> T + Send + 'static,
    ) -> (Self, Receiver<T>) {
        let (done, receipt) = async_channel::bounded(1);
        let edit = Self(Box::new(move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Closing the view drops its receipt, but the accepted write still runs"
            )]
            let _ = done.try_send(write());
        }));
        (edit, receipt)
    }
}

#[derive(Default)]
pub(crate) struct Edits {
    writing: Option<OwnedThread>,
}

impl Edits {
    #[cfg(any(
        all(test, target_os = "windows"),
        all(feature = "native-acceptance", target_os = "macos")
    ))]
    pub(crate) fn held_until(
        &mut self,
        release: Receiver<()>,
    ) -> anyhow::Result<Receiver<Result<(), async_channel::RecvError>>> {
        let (edit, receipt) = Edit::new(move || release.recv_blocking());
        self.submit(edit, None)?;
        Ok(receipt)
    }

    pub(crate) fn submit(
        &mut self,
        Edit(write): Edit,
        fixture: Option<Arc<tempfile::TempDir>>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.writing.as_ref().is_none_or(OwnedThread::is_finished),
            "A Show change is still being saved. Try again when it finishes."
        );
        self.writing = Some(OwnedThread::spawn("show-edit", move || {
            let _fixture = fixture;
            write();
        })?);
        Ok(())
    }

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
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn closing_the_view_keeps_accepted_writes_and_quit_waits_for_them() -> anyhow::Result<()>
    {
        let mut edits = Edits::default();
        let (release, wait) = std::sync::mpsc::channel();
        let (wrote, written) = async_channel::bounded(1);
        let (edit, receipt) = Edit::new(move || {
            wait.recv()?;
            wrote.try_send(())?;
            anyhow::Ok(())
        });
        edits.submit(edit, None)?;
        drop(receipt);
        let (busy, _) = Edit::new(|| ());
        assert!(edits.submit(busy, None).is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), edits.drained())
                .await
                .is_err()
        );
        release.send(())?;
        edits.drained().await;
        written.try_recv()?;
        let (idle, _) = Edit::new(|| ());
        assert!(edits.submit(idle, None).is_ok());
        Ok(())
    }
}
