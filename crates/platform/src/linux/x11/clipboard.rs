//! The X11 clipboard both insertion paths copy through, and the worker thread that serves it, with
//! focus checks, to the Wayland portal path.

use anyhow::Context;
use async_channel::{Receiver, Sender};
use x11rb::{
    protocol::xproto::{Atom, ConnectionExt as _, Window},
    xcb_ffi::XCBConnection,
};

use super::{connect, has_external_target, intern};
use crate::{InputSender, InsertPermit, OwnedThread, insertion::Reply, linux::HostSession};

/// Clipboard and focus calls can wait on Xwayland or another client, so they run on one owned
/// thread behind a bounded queue, away from the desktop service's input loop.
pub(in crate::linux) struct ClipboardWorker {
    _thread: OwnedThread,
    input: InputSender,
    requests: Sender<ClipboardRequest>,
    ready: Receiver<()>,
}

impl ClipboardWorker {
    pub(in crate::linux) fn spawn(input: InputSender) -> anyhow::Result<Self> {
        Self::spawn_with(input, Clipboard::open)
    }

    fn spawn_with<C: ClipboardAccess + 'static>(
        input: InputSender,
        open: impl FnOnce() -> anyhow::Result<C> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (requests, incoming) = async_channel::bounded::<ClipboardRequest>(1);
        let (opened, ready) = Reply::channel();
        let thread = OwnedThread::spawn("x11-clipboard", {
            let input = input.clone();
            // `opened` drops only after a failure is reported, so that report precedes the desktop
            // loop's generic setup failure.
            move || match open() {
                Ok(clipboard) => {
                    opened.send(());
                    serve_clipboard(clipboard, &incoming);
                },
                Err(error) => {
                    input.fail(format!(
                        "X11 clipboard is unavailable: {error}. Check your desktop connection, then resume."
                    ));
                },
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

    pub(in crate::linux) async fn owns(
        &self,
        owner: Window,
        permit: InsertPermit,
    ) -> anyhow::Result<bool> {
        self.ask(
            |reply| ClipboardRequest::Owns {
                owner,
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
        self.requests.send(request(reply)).await?;
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
    Owns {
        owner: Window,
        permit: InsertPermit,
        reply: Reply<anyhow::Result<bool>>,
    },
}

impl ClipboardRequest {
    fn perform(self, clipboard: &mut impl ClipboardAccess) {
        match self {
            Self::Target { permit, reply } => {
                let target = if permit.active() {
                    clipboard.external_target()
                } else {
                    Ok(false)
                };
                reply.send(target);
            },
            Self::Set {
                text,
                permit,
                reply,
            } => {
                let owner = if permit.active() {
                    clipboard.set_text(&text)
                } else {
                    Ok(None)
                };
                reply.send(owner);
            },
            Self::Owns {
                owner,
                permit,
                reply,
            } => {
                let owned = if permit.active() {
                    clipboard.owns(owner)
                } else {
                    Ok(false)
                };
                reply.send(owned);
            },
        }
    }
}

pub(super) trait ClipboardAccess {
    fn external_target(&self) -> anyhow::Result<bool>;
    fn set_text(&mut self, text: &str) -> anyhow::Result<Option<Window>>;
    fn owns(&self, owner: Window) -> anyhow::Result<bool>;
}

pub(super) struct Clipboard {
    pub(super) connection: XCBConnection,
    /// Opened on the first copy, so a worker that only checks focus never touches clipboard
    /// contents.
    native: Option<arboard::Clipboard>,
    selection: Atom,
}

impl Clipboard {
    pub(super) fn open() -> anyhow::Result<Self> {
        let (connection, _) = connect()?;
        let selection = intern(&connection, b"CLIPBOARD")?;
        Ok(Self {
            connection,
            native: None,
            selection,
        })
    }

    fn native(&mut self) -> anyhow::Result<&mut arboard::Clipboard> {
        Ok(match &mut self.native {
            Some(native) => native,
            empty => empty.insert(arboard::Clipboard::new()?),
        })
    }

    pub(super) fn owner(&self) -> anyhow::Result<Window> {
        Ok(self
            .connection
            .get_selection_owner(self.selection)?
            .reply()?
            .owner)
    }
}

impl ClipboardAccess for Clipboard {
    fn external_target(&self) -> anyhow::Result<bool> {
        has_external_target(&self.connection, HostSession::Wayland)
    }

    fn set_text(&mut self, text: &str) -> anyhow::Result<Option<Window>> {
        let native = self.native()?;
        native.set_text(text)?;
        // arboard claims the selection on its own connection without waiting for the server.
        // Reading back through it waits for that claim, so the owner this connection queries next
        // is current rather than the one the copy replaced.
        if native.get_text()? != text {
            return Ok(None);
        }
        // Another client can take the selection as soon as the copy returns, so paste needs both
        // the payload and an owner that stays put.
        let owner = self.owner()?;
        Ok(
            (owner != x11rb::NONE && self.native()?.get_text()? == text && self.owns(owner)?)
                .then_some(owner),
        )
    }

    fn owns(&self, owner: Window) -> anyhow::Result<bool> {
        Ok(owner != x11rb::NONE && self.owner()? == owner)
    }
}

fn serve_clipboard(mut clipboard: impl ClipboardAccess, requests: &Receiver<ClipboardRequest>) {
    while let Ok(request) = requests.recv_blocking() {
        request.perform(&mut clipboard);
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use anyhow::bail;
    use futures_util::FutureExt;

    use super::*;
    use crate::Input;

    #[tokio::test]
    async fn blocked_focus_query_keeps_cancellation_responsive_and_never_accesses_clipboard()
    -> anyhow::Result<()> {
        struct FocusOnly {
            entered: async_channel::Sender<()>,
            release: mpsc::Receiver<()>,
        }

        impl ClipboardAccess for FocusOnly {
            fn external_target(&self) -> anyhow::Result<bool> {
                self.entered.try_send(())?;
                self.release.recv_timeout(Duration::from_secs(2))?;
                Ok(false)
            }

            fn set_text(&mut self, _: &str) -> anyhow::Result<Option<Window>> {
                bail!("Focus accessed clipboard contents")
            }

            fn owns(&self, _: Window) -> anyhow::Result<bool> {
                bail!("Focus accessed clipboard ownership")
            }
        }

        let (sender, _events) = async_channel::bounded(4);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Missing focus permit")?;
        let (entered, blocked) = async_channel::bounded(1);
        let (release, waiting) = mpsc::channel();
        let worker = ClipboardWorker::spawn_with(input.clone(), || {
            Ok(FocusOnly {
                entered,
                release: waiting,
            })
        })?;
        worker.ready().await?;
        {
            let mut target = std::pin::pin!(worker.external_target(permit.clone()));
            tokio::select! {
                result = &mut target => bail!("Focus did not block: {result:?}"),
                started = tokio::time::timeout(Duration::from_secs(1), blocked.recv()) => {
                    started??;
                },
            }
            input.deliver(Input::Cancel);
            assert!(!permit.active());
            release.send(())?;
            assert!(!target.await?);
            assert!(!permit.commit());
        }
        drop(worker);
        Ok(())
    }

    #[tokio::test]
    async fn clipboard_setup_does_not_block_cancel_and_skips_a_stale_queued_copy()
    -> anyhow::Result<()> {
        struct FakeClipboard(mpsc::Sender<()>);

        impl ClipboardAccess for FakeClipboard {
            fn external_target(&self) -> anyhow::Result<bool> {
                Ok(false)
            }

            fn set_text(&mut self, _: &str) -> anyhow::Result<Option<Window>> {
                self.0.send(())?;
                Ok(Some(7))
            }

            fn owns(&self, owner: Window) -> anyhow::Result<bool> {
                Ok(owner == 7)
            }
        }

        let (events, _event_receiver) = async_channel::bounded(4);
        let input = InputSender::new(events);
        let previous = input.begin().context("Previous recording")?;
        let (copy_log, copies) = mpsc::channel();
        let (entered, opening) = async_channel::bounded(1);
        let (release, waiting) = mpsc::channel();
        let worker = ClipboardWorker::spawn_with(input.clone(), move || {
            entered.try_send(())?;
            waiting.recv_timeout(Duration::from_secs(2))?;
            Ok(FakeClipboard(copy_log))
        })?;
        tokio::time::timeout(Duration::from_secs(1), opening.recv()).await??;
        let mut ready = Box::pin(worker.ready());
        assert!(
            ready.as_mut().now_or_never().is_none(),
            "Desktop readiness preceded clipboard setup"
        );
        let mut previous_copy = Box::pin(worker.set_text("fixture".into(), previous.clone()));
        assert!(previous_copy.as_mut().now_or_never().is_none());
        input.deliver(Input::Cancel);
        assert!(!previous.active());
        let current = input.begin().context("Current recording")?;
        tokio::time::timeout(
            Duration::from_millis(100),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .context("Clipboard setup blocked the local runtime")?;
        release.send(())?;
        ready.await?;

        assert!(previous_copy.await?.is_none());
        assert_eq!(
            worker.set_text("fixture".into(), current.clone()).await?,
            Some(7)
        );
        assert_eq!(copies.try_iter().count(), 1);
        assert!(worker.owns(7, current.clone()).await?);
        drop(worker);
        assert!(
            !current.active(),
            "Clipboard retirement left its input generation active"
        );
        assert!(
            !input.is_closed(),
            "Clipboard retirement closed input before the desktop thread could report why"
        );
        Ok(())
    }
}
