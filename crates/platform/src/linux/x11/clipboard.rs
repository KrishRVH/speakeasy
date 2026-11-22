use std::panic::{self, AssertUnwindSafe};

use anyhow::{Context as _, ensure};
use async_channel::{Receiver, Sender};
use x11rb::xcb_ffi::XCBConnection;

use super::{connect_with_root, has_external_target};
use crate::{
    InputSender, InsertPermit, OwnedThread, clipboard::copy_when, insertion::Reply,
    linux::HostSession,
};

pub(in crate::linux) struct ClipboardWorker {
    _thread: OwnedThread,
    input: InputSender,
    requests: Sender<ClipboardRequest>,
    ready: Receiver<()>,
}

impl ClipboardWorker {
    pub(in crate::linux) fn start(input: InputSender) -> anyhow::Result<Self> {
        Self::spawn(input, Clipboard::open)
    }

    pub(in crate::linux) fn spawn<C: ClipboardAccess>(
        input: InputSender,
        open: impl FnOnce() -> anyhow::Result<C> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (requests, incoming) = async_channel::bounded(1);
        let (opened, ready) = Reply::channel();
        let thread = OwnedThread::spawn("x11-clipboard", {
            let input = input.clone();

            move || {
                let served = panic::catch_unwind(AssertUnwindSafe(|| {
                    serve_clipboard(open, opened, &incoming)
                }));
                match served {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => input.fail(format!(
                        "X11 clipboard is unavailable: {error}. Check your desktop connection, then resume dictation."
                    )),
                    Err(_) => input.fail(
                        "X11 clipboard stopped unexpectedly. Resume dictation to reconnect.".into(),
                    ),
                }
            }
        })?;
        Ok(Self {
            _thread: thread,
            input,
            requests,
            ready,
        })
    }

    pub(in crate::linux) async fn ready(&self) -> anyhow::Result<()> {
        self.ready.recv().await.context("Clipboard setup stopped")
    }

    pub(in crate::linux) async fn external_target(
        &self,
        permit: InsertPermit,
    ) -> anyhow::Result<bool> {
        self.ask(
            |reply| ClipboardRequest::Target { permit, reply },
            "Target focus check stopped",
        )
        .await
    }

    pub(in crate::linux) async fn set_text(
        &self,
        text: String,
        permit: InsertPermit,
    ) -> anyhow::Result<bool> {
        self.ask(
            |reply| ClipboardRequest::Set {
                text,
                permit,
                reply,
            },
            "Clipboard preparation stopped",
        )
        .await
    }

    async fn ask<T>(
        &self,
        request: impl FnOnce(Reply<anyhow::Result<T>>) -> ClipboardRequest,
        stopped: &'static str,
    ) -> anyhow::Result<T> {
        let (reply, answer) = Reply::channel();
        self.requests
            .send(request(reply))
            .await
            .map_err(|_closed| anyhow::anyhow!(stopped))?;
        answer.recv().await.context(stopped)?
    }
}

impl Drop for ClipboardWorker {
    fn drop(&mut self) {
        self.input.cancel();
        self.requests.close();
    }
}

enum ClipboardRequest {
    Target {
        permit: InsertPermit,
        reply: Reply<anyhow::Result<bool>>,
    },
    Set {
        text: String,
        permit: InsertPermit,
        reply: Reply<anyhow::Result<bool>>,
    },
}

impl ClipboardRequest {
    fn perform(self, clipboard: &mut impl ClipboardAccess) {
        match self {
            Self::Target { permit, reply } => answer(&permit, reply, false, || {
                clipboard.external_target(HostSession::Wayland)
            }),
            Self::Set {
                text,
                permit,
                reply,
            } => answer(&permit, reply, false, || clipboard.set_text(&text, &permit)),
        }
    }
}

fn answer<T>(
    permit: &InsertPermit,
    reply: Reply<anyhow::Result<T>>,
    idle: T,
    work: impl FnOnce() -> anyhow::Result<T>,
) {
    reply.send(if permit.active() { work() } else { Ok(idle) });
}

pub(in crate::linux) trait ClipboardAccess {
    fn external_target(&self, host: HostSession) -> anyhow::Result<bool>;
    fn set_text(&mut self, text: &str, permit: &InsertPermit) -> anyhow::Result<bool>;
}

pub(super) struct Clipboard {
    pub(super) connection: XCBConnection,
    native: Option<arboard::Clipboard>,
}

impl Clipboard {
    pub(super) fn open() -> anyhow::Result<Self> {
        let (connection, _) = connect_with_root()?;
        Ok(Self {
            connection,
            native: None,
        })
    }

    fn native(&mut self) -> anyhow::Result<&mut arboard::Clipboard> {
        Ok(match &mut self.native {
            Some(native) => native,
            empty => empty.insert(arboard::Clipboard::new()?),
        })
    }
}

impl ClipboardAccess for Clipboard {
    fn external_target(&self, host: HostSession) -> anyhow::Result<bool> {
        has_external_target(&self.connection, host)
    }

    fn set_text(&mut self, text: &str, permit: &InsertPermit) -> anyhow::Result<bool> {
        copy_when(
            || permit.active(),
            || self.native(),
            |native| {
                native.set_text(text)?;
                ensure!(
                    native.get_text()? == text,
                    "The X11 clipboard did not keep the copied text. Try Paste last dictation again."
                );
                Ok(())
            },
        )
    }
}

fn serve_clipboard<C: ClipboardAccess>(
    open: impl FnOnce() -> anyhow::Result<C>,
    opened: Reply<()>,
    requests: &Receiver<ClipboardRequest>,
) -> anyhow::Result<()> {
    let mut clipboard = open()?;
    opened.send(());
    while let Ok(request) = requests.recv_blocking() {
        request.perform(&mut clipboard);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
