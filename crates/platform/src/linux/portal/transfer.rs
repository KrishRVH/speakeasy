use std::{
    fs::File,
    io::{self, Write as _},
    os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd},
    sync::Arc,
    time::Duration,
};

use anyhow::Context as _;
use tokio::io::{Interest, unix::AsyncFd};
use zbus::{Proxy, zvariant::OwnedObjectPath};

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) async fn transfer(
    clipboard: &Proxy<'static>,
    session: OwnedObjectPath,
    serial: u32,
    text: Option<Arc<str>>,
) {
    let request = (&session, serial);
    let write = clipboard.call::<_, _, zbus::zvariant::OwnedFd>("SelectionWrite", &request);
    // With nothing to serve, still take the descriptor and close it empty: the compositor waits
    // for it to close.
    let success = match (tokio::time::timeout(TRANSFER_TIMEOUT, write).await, text) {
        (Ok(Ok(fd)), Some(text)) => {
            matches!(
                tokio::time::timeout(TRANSFER_TIMEOUT, write_payload(fd.into(), &text)).await,
                Ok(Ok(()))
            )
        },
        _ => false,
    };
    let report = (&session, serial, success);
    let done = clipboard.call::<_, _, ()>("SelectionWriteDone", &report);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "The portal may drop an individual transfer; the session monitor reports a lost portal"
    )]
    let _ = tokio::time::timeout(TRANSFER_TIMEOUT, done).await;
}

async fn write_payload(fd: OwnedFd, text: &str) -> anyhow::Result<()> {
    // Nonblocking, so a full pipe yields to the desktop thread's loop instead of stalling it.
    set_nonblocking(fd.as_fd())?;
    // SAFETY: the `File` owns the descriptor, keeping it open and unchanged until `pipe` drops.
    let pipe = unsafe { AsyncFd::register(File::from(fd)) }?;
    let mut bytes = text.as_bytes();
    while !bytes.is_empty() {
        let written = pipe
            .async_io(Interest::WRITABLE, |mut pipe| pipe.write(bytes))
            .await?;
        if written == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero))
                .context("Clipboard transfer stopped");
        }
        bytes = bytes
            .get(written..)
            .context("Clipboard write exceeded its pending transfer")?;
    }
    Ok(())
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> anyhow::Result<()> {
    let fd = fd.as_raw_fd();
    let result = unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            flags
        } else {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK)
        }
    };
    if result < 0 {
        return Err(io::Error::last_os_error()).context("Cannot prepare clipboard transfer");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, pipe};

    use super::*;

    #[tokio::test]
    async fn a_clipboard_transfer_writes_utf8_and_closes_its_writer() -> anyhow::Result<()> {
        let text = "Café 🌿";
        let (mut reader, writer) = pipe()?;
        write_payload(writer.into(), text).await?;
        let mut received = String::new();
        reader.read_to_string(&mut received)?;
        assert_eq!(received, text);
        Ok(())
    }
}
