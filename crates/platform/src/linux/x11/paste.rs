use std::{
    ops::ControlFlow,
    panic::{self, AssertUnwindSafe},
};

use anyhow::{Context as _, ensure};
use async_channel::Receiver;
use x11rb::{
    protocol::{
        xkb,
        xkb::ConnectionExt as _,
        xproto::{self, ConnectionExt as _, Window},
        xtest::ConnectionExt as _,
    },
    xcb_ffi::XCBConnection,
};

use super::{
    Keyboard,
    clipboard::{Clipboard, ClipboardAccess as _},
};
use crate::{
    Delivery, InputSender, Inserted, LinuxPaste, OwnedThread,
    insertion::{
        Insertion, PASTE_AFTER_RELEASE, Reply, abandon_queued, wait_for_released_modifiers,
    },
    linux::{
        CLIPBOARD_CHANGED, COPIED_FOR_MANUAL_PASTE, HostSession, NO_PASTE_BINDING, PasteChord,
    },
};

pub(super) struct InsertionWorker {
    _thread: OwnedThread,
    input: InputSender,
    requests: Receiver<Insertion>,
    ready: Receiver<()>,
}

impl InsertionWorker {
    pub(super) fn start(
        input: InputSender,
        paste: LinuxPaste,
        requests: Receiver<Insertion>,
    ) -> anyhow::Result<Self> {
        Self::spawn(input, requests, move || Paster::open(paste), Paster::serve)
    }

    fn spawn<S>(
        input: InputSender,
        requests: Receiver<Insertion>,
        open: impl FnOnce() -> anyhow::Result<S> + Send + 'static,
        serve: impl FnOnce(S, &Receiver<Insertion>) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (opened, ready) = Reply::channel();
        let thread = OwnedThread::spawn("x11-insertion", {
            let (input, requests) = (input.clone(), requests.clone());
            move || {
                let served = match panic::catch_unwind(AssertUnwindSafe(open)) {
                    Ok(Ok(state)) => {
                        opened.send(());
                        panic::catch_unwind(AssertUnwindSafe(|| serve(state, &requests))).map(Ok)
                    },
                    Ok(Err(error)) => Ok(Err(error)),
                    Err(panic) => Err(panic),
                };
                match served {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => input.fail(format!(
                        "X11 input is unavailable: {error}. Check your desktop connection, then resume dictation."
                    )),
                    Err(_) => input.fail(
                        "X11 input stopped unexpectedly. Resume dictation to reconnect.".into(),
                    ),
                }
                abandon_queued(&requests);
            }
        })?;
        Ok(Self {
            _thread: thread,
            input,
            requests,
            ready,
        })
    }

    pub(super) async fn ready(&self) -> anyhow::Result<()> {
        self.ready.recv().await.context("X11 input setup stopped")
    }
}

impl Drop for InsertionWorker {
    fn drop(&mut self) {
        self.input.cancel();
        self.requests.close();
    }
}

struct Paster {
    clipboard: Clipboard,
    keyboard: Keyboard,
    paste: LinuxPaste,
}

impl Paster {
    fn open(paste: LinuxPaste) -> anyhow::Result<Self> {
        let clipboard = Clipboard::open()?;
        let keyboard = Keyboard::load(&clipboard.connection)?;
        Ok(Self {
            clipboard,
            keyboard,
            paste,
        })
    }

    fn serve(mut self, requests: &Receiver<Insertion>) {
        while let Ok(request) = requests.recv_blocking() {
            let outcome = self.paste(&request);
            request.reply.send(outcome);
        }
    }

    fn paste(&mut self, request: &Insertion) -> anyhow::Result<Inserted> {
        if !request.permit.active() {
            return Ok(Inserted::Cancelled);
        }
        if request.delivery == Delivery::Direct {
            return Ok(Inserted::Unavailable(
                "Direct text input is unavailable on this X11 desktop. Set Insert text by to a Paste option in Settings.",
            ));
        }
        match self.copy_after_release(request)? {
            ControlFlow::Continue(owner) => self.paste_copied(request, owner),
            ControlFlow::Break(outcome) => Ok(outcome),
        }
    }

    fn copy_after_release(
        &mut self,
        request: &Insertion,
    ) -> anyhow::Result<ControlFlow<Inserted, Window>> {
        let previous_owner = self.clipboard.owner()?;
        let shortcut_held = self.paste != LinuxPaste::Manual
            && wait_for_released_modifiers(&request.permit, || self.modifiers_held())?;
        let owner_changed = self.clipboard.owner()? != previous_owner;

        if !request.permit.active() {
            return Ok(ControlFlow::Break(Inserted::Cancelled));
        }
        if owner_changed {
            return Ok(ControlFlow::Break(Inserted::Unavailable(
                "The clipboard changed while your shortcut keys were held. Release them before pasting.",
            )));
        }
        let Some(owner) = self.clipboard.set_text(&request.text, &request.permit)? else {
            return Ok(ControlFlow::Break(if request.permit.active() {
                CLIPBOARD_CHANGED
            } else {
                Inserted::Cancelled
            }));
        };
        if self.paste == LinuxPaste::Manual {
            return Ok(ControlFlow::Break(COPIED_FOR_MANUAL_PASTE));
        }
        if shortcut_held {
            return Ok(ControlFlow::Break(PASTE_AFTER_RELEASE));
        }
        Ok(ControlFlow::Continue(owner))
    }

    fn modifiers_held(&self) -> anyhow::Result<bool> {
        let keys = self.clipboard.connection.query_keymap()?.reply()?.keys;
        Ok(self.keyboard.held_modifiers(&keys) & !self.keyboard.lock_modifiers() != 0)
    }

    fn paste_copied(&self, request: &Insertion, owner: Window) -> anyhow::Result<Inserted> {
        let Some(chord) =
            PasteChord::resolve(self.paste, |symbol| self.keyboard.unshifted_keycode(symbol))
        else {
            return Ok(NO_PASTE_BINDING);
        };
        let connection = &self.clipboard.connection;
        if !uses_first_layout(connection).unwrap_or(false) {
            return Ok(Inserted::Copied(
                "Text copied. Automatic paste needs your first keyboard layout; paste manually with the current layout.",
            ));
        }
        if !self.clipboard.owns(owner)? {
            return Ok(CLIPBOARD_CHANGED);
        }
        let external = !request.own_focus.held()
            && self
                .clipboard
                .external_target(HostSession::X11)
                .unwrap_or(false);

        let released = !self.modifiers_held()?;
        if let Some(outcome) = Delivery::Paste.preflight(external, released) {
            return Ok(outcome);
        }
        if !self.clipboard.owns(owner)? {
            return Ok(CLIPBOARD_CHANGED);
        }
        if !request.permit.commit() {
            return Ok(Inserted::Cancelled);
        }

        let submitted = submit_paste(
            chord,
            |kind, code| Ok(connection.xtest_fake_input(kind, code, 0, 0, 0, 0, 0)?),
            || {
                connection.get_input_focus()?.reply()?;
                Ok(())
            },
            |cookie| Ok(cookie.check()?),
        );
        Ok(match submitted {
            Ok(()) => Inserted::Sent,
            Err(_) => Inserted::Copied(
                "Text copied, but automatic paste failed and was not retried. Paste it manually.",
            ),
        })
    }
}

fn uses_first_layout(connection: &XCBConnection) -> anyhow::Result<bool> {
    ensure!(
        connection.xkb_use_extension(1, 0)?.reply()?.supported,
        "XKB is unavailable"
    );
    let state = connection
        .xkb_get_state(xkb::ID::USE_CORE_KBD.into())?
        .reply()?;
    Ok(state.group == xkb::Group::M1)
}

fn submit_paste<C>(
    chord: PasteChord<u8>,
    mut enqueue: impl FnMut(u8, u8) -> anyhow::Result<C>,
    synchronize: impl FnOnce() -> anyhow::Result<()>,
    mut check: impl FnMut(C) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut cookies = Vec::with_capacity(6);
    let mut failed = false;
    for code in chord.presses() {
        let Ok(cookie) = enqueue(xproto::KEY_PRESS_EVENT, code) else {
            failed = true;
            break;
        };
        cookies.push(cookie);
    }

    for code in chord.presses().rev() {
        match enqueue(xproto::KEY_RELEASE_EVENT, code) {
            Ok(cookie) => cookies.push(cookie),
            Err(_) => failed = true,
        }
    }
    synchronize()?;
    for cookie in cookies {
        failed |= check(cookie).is_err();
    }
    ensure!(!failed, "Input submission failed");
    Ok(())
}

#[cfg(test)]
mod tests;
