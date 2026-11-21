use std::panic::{self, AssertUnwindSafe};

use anyhow::Context as _;
use async_channel::{Receiver, Sender};
use x11rb::{
    connection::Connection as _,
    protocol::{
        Event,
        xfixes::{ConnectionExt as _, SelectionEventMask},
        xproto::{Atom, ConnectionExt as _, Window},
    },
    xcb_ffi::XCBConnection,
};

use super::{connect_with_root, has_external_target, intern};
use crate::{
    InputSender, InsertPermit, OwnedThread,
    clipboard::copy_when,
    insertion::{Reply, holds_text},
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
    ) -> anyhow::Result<Option<Window>> {
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

    pub(in crate::linux) async fn paste_target(
        &self,
        owner: Window,
        text: String,
        permit: InsertPermit,
    ) -> anyhow::Result<(bool, bool)> {
        self.ask(
            |reply| ClipboardRequest::PasteTarget {
                owner,
                text,
                permit,
                reply,
            },
            "Clipboard ownership monitoring stopped",
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
        reply: Reply<anyhow::Result<Option<Window>>>,
    },
    PasteTarget {
        owner: Window,
        text: String,
        permit: InsertPermit,
        reply: Reply<anyhow::Result<(bool, bool)>>,
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
            } => answer(&permit, reply, None, || clipboard.set_text(&text, &permit)),
            Self::PasteTarget {
                owner,
                text,
                permit,
                reply,
            } => answer(&permit, reply, (false, false), || {
                let external = clipboard
                    .external_target(HostSession::Wayland)
                    .unwrap_or(false);
                let owned = permit.active() && clipboard.holds(owner, &text)?;
                Ok((external, owned))
            }),
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
    fn write_text(&mut self, text: &str, permit: &InsertPermit) -> anyhow::Result<bool>;
    fn owner(&self) -> anyhow::Result<Window>;
    fn holds_text(&mut self, text: &str) -> anyhow::Result<bool>;

    fn set_text(&mut self, text: &str, permit: &InsertPermit) -> anyhow::Result<Option<Window>> {
        if !self.write_text(text, permit)? || !self.holds_text(text)? {
            return Ok(None);
        }
        let owner = self.owner()?;
        Ok((self.holds(owner, text)? && permit.active()).then_some(owner))
    }

    fn holds(&mut self, owner: Window, text: &str) -> anyhow::Result<bool> {
        Ok(owner != x11rb::NONE && self.holds_text(text)? && self.owner()? == owner)
    }
}

pub(super) struct Clipboard {
    pub(super) connection: XCBConnection,
    native: Option<arboard::Clipboard>,
    root: Window,
    selection: Atom,
}

impl Clipboard {
    pub(super) fn open() -> anyhow::Result<Self> {
        let (connection, root) = connect_with_root()?;
        connection.xfixes_query_version(5, 0)?.reply()?;
        let selection = intern(&connection, b"CLIPBOARD")?;
        Ok(Self {
            connection,
            native: None,
            root,
            selection,
        })
    }

    pub(super) fn watching_owner<T>(
        &self,
        during: impl FnOnce(&Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<(T, bool)> {
        self.watch_owner(
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )?;
        self.owner_changed()?;
        let during = during(self);
        let stopped = self.watch_owner(SelectionEventMask::default());
        let changed = self.owner_changed()?;
        stopped?;
        Ok((during?, changed))
    }

    fn watch_owner(&self, changes: SelectionEventMask) -> anyhow::Result<()> {
        self.connection
            .xfixes_select_selection_input(self.root, self.selection, changes)?
            .check()?;
        Ok(())
    }

    fn owner_changed(&self) -> anyhow::Result<bool> {
        let mut changed = false;
        while let Some(event) = self.connection.poll_for_event()? {
            changed |= matches!(
                event,
                Event::XfixesSelectionNotify(notify) if notify.selection == self.selection
            );
        }
        Ok(changed)
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

    fn write_text(&mut self, text: &str, permit: &InsertPermit) -> anyhow::Result<bool> {
        copy_when(
            || permit.active(),
            || self.native(),
            |native| Ok(native.set_text(text)?),
        )
    }

    fn owner(&self) -> anyhow::Result<Window> {
        Ok(self
            .connection
            .get_selection_owner(self.selection)?
            .reply()?
            .owner)
    }

    fn holds_text(&mut self, text: &str) -> anyhow::Result<bool> {
        holds_text(self.native()?.get_text(), text)
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
