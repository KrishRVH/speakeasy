use std::{path::PathBuf, sync::Arc};

use async_channel::Receiver;
use speakeasy_platform::OwnedThread;

use super::{
    Interrupted,
    store::{
        export,
        retention::{self, Retention},
        video,
    },
};

pub(crate) enum Edit {
    Keep {
        parent: PathBuf,
        directory: PathBuf,
        kept: bool,
    },
    Discard(Interrupted),
    Export {
        directory: PathBuf,
        destination: PathBuf,
    },
    Video {
        directory: PathBuf,
        destination: PathBuf,
        cancel: Receiver<()>,
    },
}

pub(crate) enum Written {
    Retention(Retention),
    Discarded,
    Exported(export::Exported),
    Video(video::Exported),
}

pub(crate) type Receipt = Receiver<anyhow::Result<Written>>;

#[derive(Default)]
pub(crate) struct Edits {
    writing: Option<OwnedThread>,
}

impl Edits {
    #[cfg(any(
        all(test, target_os = "windows"),
        all(feature = "native-acceptance", target_os = "macos")
    ))]
    pub(crate) fn held_until(&mut self, release: Receiver<()>) -> anyhow::Result<Receipt> {
        self.start(move || {
            release.recv_blocking()?;
            Ok(Written::Discarded)
        })
    }

    pub(crate) fn submit(
        &mut self,
        edit: Edit,
        fixture: Option<Arc<tempfile::TempDir>>,
    ) -> anyhow::Result<Receipt> {
        self.start(move || {
            let _fixture = fixture;
            match edit {
                Edit::Keep {
                    parent,
                    directory,
                    kept,
                } => retention::set_kept(&parent, &directory, kept)
                    .map(Written::Retention)
                    .map_err(Into::into),
                Edit::Discard(interrupted) => {
                    interrupted.lock()?.discard().map(|()| Written::Discarded)
                },
                Edit::Export {
                    directory,
                    destination,
                } => export::write(&directory, &destination).map(Written::Exported),
                Edit::Video {
                    directory,
                    destination,
                    cancel,
                } => video::write(
                    &directory,
                    &destination,
                    video::Options {
                        captions: true,
                        ..video::Options::default()
                    },
                    || cancel.is_closed(),
                )
                .map(Written::Video),
            }
        })
    }

    fn start(
        &mut self,
        write: impl FnOnce() -> anyhow::Result<Written> + Send + 'static,
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
            Ok(Written::Discarded)
        })?;
        drop(result);
        assert!(edits.start(|| Ok(Written::Discarded)).is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), edits.drained())
                .await
                .is_err()
        );
        release.send(())?;
        edits.drained().await;
        written.try_recv()?;
        assert!(edits.start(|| Ok(Written::Discarded)).is_ok());
        Ok(())
    }
}
