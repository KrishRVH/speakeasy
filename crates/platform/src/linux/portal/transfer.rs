//! Portal clipboard transfers: the text a selection serves, and bounded writes of it to the pipe
//! each pasting editor opens.

use std::{
    convert::Infallible,
    fs::File,
    io::Write,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, ensure};
use async_channel::{Receiver, Sender};
use futures_util::{future::LocalBoxFuture, stream::FuturesUnordered};
use tokio::io::{Interest, unix::AsyncFd};

use crate::InsertPermit;

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) type Transfers<'a> = FuturesUnordered<LocalBoxFuture<'a, ()>>;

/// The text a portal clipboard selection serves until it is replaced or this session loses the
/// selection.
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

    /// Whether this selection holds the text of `permit`'s recording.
    pub(super) fn serves(&self, permit: &InsertPermit) -> bool {
        self.permit.same_recording(permit)
    }
}

impl Drop for Selection {
    fn drop(&mut self) {
        self.invalidate.close();
    }
}

/// One transfer's share of a selection's text, withdrawn once the selection drops.
pub(super) struct TransferPayload {
    text: Arc<str>,
    invalidated: Receiver<Infallible>,
}

/// Answers one `SelectionTransfer`: writes `payload` to the descriptor `write` yields unless its
/// selection drops first, then reports through `done` whether the whole payload reached the pipe.
/// A missing payload reports failure. Each step is bounded by [`TRANSFER_TIMEOUT`].
///
/// Every failure, including a refused or stalled report, fails only this transfer: the editor reads
/// nothing. A lost portal or bus connection ends serving through the session monitor instead.
pub(super) async fn transfer<D: Future<Output = anyhow::Result<()>>>(
    write: impl Future<Output = anyhow::Result<OwnedFd>>,
    done: impl FnOnce(bool) -> D,
    payload: Option<TransferPayload>,
) {
    // Editors may request the payload after paste commits, so only selection loss ends serving.
    let success = match payload {
        Some(payload) => tokio::select! {
            biased;
            _ = payload.invalidated.recv() => false,
            written = write_payload(write, &payload.text) => written.is_ok(),
        },
        None => false,
    };
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A report the portal refuses or never answers fails only this transfer; the session monitor reports a lost portal"
    )]
    let _ = tokio::time::timeout(TRANSFER_TIMEOUT, done(success)).await;
}

async fn write_payload(
    write: impl Future<Output = anyhow::Result<OwnedFd>>,
    text: &str,
) -> anyhow::Result<()> {
    let fd = tokio::time::timeout(TRANSFER_TIMEOUT, write).await??;
    set_nonblocking(fd.as_fd())?;
    // SAFETY: the `File` owns the pipe's descriptor and keeps it open and unchanged until the
    // `AsyncFd` drops it.
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
    // SAFETY: the borrowed descriptor stays open; F_GETFL and F_SETFL only read and change its
    // status flags.
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
    use futures_util::{FutureExt, StreamExt};

    use super::*;
    use crate::InputSender;

    #[tokio::test]
    async fn stalled_transfers_allow_new_work_and_retire_without_detached_writes()
    -> anyhow::Result<()> {
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
                |_| async { Ok(()) },
                Some(selection.payload()),
            )
            .boxed_local(),
        );
        assert!(transfers.next().now_or_never().is_none());
        opened.recv().await?;
        let (other_work, incoming) = async_channel::bounded(1);
        other_work.send(()).await?;
        tokio::select! {
            result = incoming.recv() => result?,
            _ = transfers.next() => bail!("Stalled transfer unexpectedly completed"),
        }
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
        let (sent, reported) = async_channel::bounded(1);
        transfer(
            async { Ok(OwnedFd::from(writer)) },
            move |success| async move {
                sent.send(success).await?;
                Ok(())
            },
            Some(selected.payload()),
        )
        .await;
        assert!(
            reported.recv().await?,
            "Paste commitment invalidated clipboard serving"
        );
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"fixture");

        let current = input.begin().context("Current recording")?;
        let next = Selection::new("fixture", current.clone());
        let previous_payload = selected.payload();
        drop(selected);
        let (sent, reported) = async_channel::bounded(1);
        let (writer, mut reader) = UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_millis(100)))?;
        transfer(
            async { Ok(OwnedFd::from(writer)) },
            move |success| async move {
                sent.send(success).await?;
                Ok(())
            },
            Some(previous_payload),
        )
        .await;
        assert!(!reported.recv().await?);
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
        let (sent, reported) = async_channel::bounded(1);
        tokio::time::timeout(
            TRANSFER_TIMEOUT + Duration::from_millis(500),
            transfer(
                std::future::pending(),
                move |success| async move {
                    sent.send(success).await?;
                    Ok(())
                },
                Some(selected.payload()),
            ),
        )
        .await?;
        assert!(!reported.recv().await?);
        tokio::time::timeout(
            TRANSFER_TIMEOUT + Duration::from_millis(500),
            transfer(std::future::pending(), |_| std::future::pending(), None),
        )
        .await
        .context("SelectionWriteDone had no deadline")?;
        Ok(())
    }
}
