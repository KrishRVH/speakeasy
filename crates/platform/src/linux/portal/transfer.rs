use std::{
    convert::Infallible,
    fs::File,
    io::Write as _,
    os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, ensure};
use async_channel::{Receiver, Sender};
use futures_util::{future::LocalBoxFuture, stream::FuturesUnordered};
use tokio::io::{Interest, unix::AsyncFd};

use crate::InsertPermit;

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) type Transfers<'a> = FuturesUnordered<LocalBoxFuture<'a, ()>>;

pub(super) struct Selection {
    text: Arc<str>,
    permit: InsertPermit,
    invalidate: Sender<Infallible>,
    invalidated: Receiver<Infallible>,
}

impl Selection {
    pub(super) fn new(text: &str, permit: InsertPermit) -> Self {
        let (invalidate, invalidated) = async_channel::bounded(1);
        Self {
            text: text.into(),
            permit,
            invalidate,
            invalidated,
        }
    }

    pub(super) fn payload(&self) -> TransferPayload {
        TransferPayload {
            text: Arc::clone(&self.text),
            invalidated: self.invalidated.clone(),
        }
    }

    pub(super) fn serves(&self, permit: &InsertPermit) -> bool {
        self.permit.same_recording(permit)
    }
}

impl Drop for Selection {
    fn drop(&mut self) {
        self.invalidate.close();
    }
}

pub(super) struct TransferPayload {
    text: Arc<str>,
    invalidated: Receiver<Infallible>,
}

pub(super) async fn transfer(
    write: impl Future<Output = anyhow::Result<OwnedFd>>,
    done: impl AsyncFnOnce(bool) -> anyhow::Result<()>,
    payload: TransferPayload,
) {
    let success = match tokio::time::timeout(TRANSFER_TIMEOUT, write).await {
        Ok(Ok(fd)) => tokio::select! {
            biased;
            _ = payload.invalidated.recv() => false,
            written = write_payload(fd, &payload.text) => written.is_ok(),
        },
        Ok(Err(_)) | Err(_) => false,
    };
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A report the portal refuses or never answers fails only this transfer; the session monitor reports a lost portal"
    )]
    let _ = tokio::time::timeout(TRANSFER_TIMEOUT, done(success)).await;
}

pub(super) async fn refuse(
    write: impl Future<Output = anyhow::Result<OwnedFd>>,
    done: impl AsyncFnOnce(bool) -> anyhow::Result<()>,
) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Dropping the descriptor ends the reader's transfer empty; a refused or unanswered write leaves only the failure report"
    )]
    let _ = tokio::time::timeout(TRANSFER_TIMEOUT, write).await;
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A report the portal refuses or never answers fails only this transfer; the session monitor reports a lost portal"
    )]
    let _ = tokio::time::timeout(TRANSFER_TIMEOUT, done(false)).await;
}

async fn write_payload(fd: OwnedFd, text: &str) -> anyhow::Result<()> {
    set_nonblocking(fd.as_fd())?;

    let pipe = unsafe { AsyncFd::register(File::from(fd)) }?;
    tokio::time::timeout(TRANSFER_TIMEOUT, write_to_pipe(&pipe, text.as_bytes())).await?
}

async fn write_to_pipe(pipe: &AsyncFd<File>, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        let written = pipe
            .async_io(Interest::WRITABLE, |mut pipe| pipe.write(bytes))
            .await?;
        ensure!(written > 0, "Clipboard transfer stopped");
        bytes = bytes
            .get(written..)
            .context("Clipboard write exceeded its pending transfer")?;
    }
    Ok(())
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> anyhow::Result<()> {
    let fd = fd.as_raw_fd();

    let updated = unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
    };
    ensure!(updated, "Cannot prepare clipboard transfer");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Read as _},
        os::unix::net::UnixStream,
    };

    use anyhow::bail;
    use futures_util::{FutureExt as _, StreamExt as _};

    use super::*;
    use crate::InputSender;

    async fn reported_success(
        write: impl Future<Output = anyhow::Result<OwnedFd>>,
        payload: TransferPayload,
    ) -> anyhow::Result<bool> {
        let (sent, reported) = async_channel::bounded(1);
        transfer(
            write,
            async move |success| {
                sent.send(success).await?;
                Ok(())
            },
            payload,
        )
        .await;
        Ok(reported.recv().await?)
    }

    #[tokio::test]
    async fn a_retired_stalled_transfer_releases_its_writer() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        let selection = Selection::new(&"fixture".repeat(128 * 1024), permit);
        let (writer, mut reader) = UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_millis(100)))?;
        let (entered, opened) = async_channel::bounded(1);
        let mut transfers = Transfers::new();
        transfers.push(
            transfer(
                async move {
                    entered.try_send(())?;
                    Ok(OwnedFd::from(writer))
                },
                async |_| Ok(()),
                selection.payload(),
            )
            .boxed_local(),
        );
        assert!(transfers.next().now_or_never().is_none());
        opened.recv().await?;
        drop(transfers);
        assert!(
            io::copy(&mut reader, &mut io::sink()).is_ok(),
            "Retired transfer kept its native writer alive"
        );
        Ok(())
    }

    #[tokio::test]
    async fn clipboard_transfer_survives_paste_commit_and_only_its_selection_invalidates_it()
    -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let previous = input.begin().context("Previous recording")?;
        let selected = Selection::new("fixture", previous.clone());
        assert!(previous.commit());
        let (writer, mut reader) = UnixStream::pair()?;
        assert!(
            reported_success(async { Ok(OwnedFd::from(writer)) }, selected.payload()).await?,
            "Paste commitment invalidated clipboard serving"
        );
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"fixture");

        let current = input.begin().context("Current recording")?;
        let next = Selection::new("fixture", current.clone());
        let previous_payload = selected.payload();
        drop(selected);
        let (writer, mut reader) = UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_millis(100)))?;
        let (asked, requested) = async_channel::bounded(1);
        let write = async move {
            asked.try_send(())?;
            Ok(OwnedFd::from(writer))
        };
        assert!(!reported_success(write, previous_payload).await?);
        assert!(
            requested.try_recv().is_ok(),
            "An invalidated transfer never asked the portal for its descriptor"
        );
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        assert!(
            bytes.is_empty(),
            "Invalidated selection still wrote clipboard data"
        );
        assert!(
            current.active(),
            "An older transfer revoked the new recording"
        );
        assert!(
            !next.invalidated.is_closed(),
            "An older transfer invalidated the new clipboard selection"
        );
        Ok(())
    }

    #[tokio::test]
    async fn stalled_transfer_rpcs_are_bounded() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let selected = Selection::new("fixture", input.begin().context("Recording")?);
        let reported = tokio::time::timeout(
            TRANSFER_TIMEOUT + Duration::from_millis(500),
            reported_success(std::future::pending(), selected.payload()),
        )
        .await??;
        assert!(!reported);
        tokio::time::timeout(
            TRANSFER_TIMEOUT + Duration::from_millis(500),
            transfer(
                async { bail!("Fixture write failed") },
                async |_| std::future::pending().await,
                selected.payload(),
            ),
        )
        .await
        .context("SelectionWriteDone had no deadline")?;
        Ok(())
    }
}
