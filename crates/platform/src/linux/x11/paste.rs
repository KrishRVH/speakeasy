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
    Delivery, InputSender, InsertPermit, Inserted, LinuxPaste, OwnedThread,
    insertion::{
        Insertion, MODIFIER_WAIT, PASTE_AFTER_RELEASE, Reply, abandon_queued,
        wait_for_released_modifiers,
    },
    linux::{
        CLIPBOARD_CHANGED, COPIED_FOR_MANUAL_PASTE, HostSession, NO_PASTE_BINDING, PasteChord,
        PasteKeys,
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
            let outcome = self.insert(&request);
            request.reply.send(outcome);
        }
    }

    fn insert(&mut self, request: &Insertion) -> anyhow::Result<Inserted> {
        if !request.permit.active() {
            return Ok(Inserted::Cancelled);
        }
        if request.delivery == Delivery::Direct {
            return Ok(Inserted::Unavailable(
                "Direct text input is unavailable on this X11 desktop. Set “Insert text by” to a Paste option in Settings.",
            ));
        }
        match self.copy_after_release(request)? {
            ControlFlow::Continue((owner, keys)) => self.paste_copied(request, owner, keys),
            ControlFlow::Break(outcome) => Ok(outcome),
        }
    }

    fn copy_after_release(
        &mut self,
        request: &Insertion,
    ) -> anyhow::Result<ControlFlow<Inserted, (Window, PasteKeys)>> {
        let keys = PasteKeys::of(self.paste);
        let previous_owner = self.clipboard.owner()?;
        let shortcut_held = keys.is_some()
            && wait_for_released_modifiers(&request.permit, MODIFIER_WAIT, || {
                modifiers_held(&self.clipboard, &self.keyboard)
            })?;
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
        let Some(keys) = keys else {
            return Ok(ControlFlow::Break(settle_copy(
                &request.permit,
                COPIED_FOR_MANUAL_PASTE,
            )));
        };
        if shortcut_held {
            return Ok(ControlFlow::Break(settle_copy(
                &request.permit,
                PASTE_AFTER_RELEASE,
            )));
        }
        Ok(ControlFlow::Continue((owner, keys)))
    }

    fn paste_copied(
        &mut self,
        request: &Insertion,
        owner: Window,
        keys: PasteKeys,
    ) -> anyhow::Result<Inserted> {
        let Some(chord) =
            PasteChord::resolve(keys, |symbol| self.keyboard.unshifted_keycode(symbol))
        else {
            return Ok(NO_PASTE_BINDING);
        };
        if !uses_first_layout(&self.clipboard.connection).unwrap_or(false) {
            return Ok(Inserted::Copied(
                "Text copied. Automatic paste needs your first keyboard layout; paste manually with the current layout.",
            ));
        }
        if !self.clipboard.holds(owner, &request.text)? {
            return Ok(CLIPBOARD_CHANGED);
        }
        let keyboard = &self.keyboard;
        if let ControlFlow::Break(outcome) = authorize_paste(
            request,
            &mut self.clipboard,
            |clipboard| clipboard.external_target(HostSession::X11).unwrap_or(false),
            |clipboard| modifiers_held(clipboard, keyboard),
            |clipboard| clipboard.holds(owner, &request.text),
        )? {
            return Ok(outcome);
        }

        let connection = &self.clipboard.connection;
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

fn settle_copy(permit: &InsertPermit, outcome: Inserted) -> Inserted {
    match outcome {
        Inserted::Copied(_) if !permit.commit() => Inserted::Cancelled,
        Inserted::Unavailable(_) if !permit.active() => Inserted::Cancelled,
        outcome => outcome,
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

fn modifiers_held(clipboard: &Clipboard, keyboard: &Keyboard) -> anyhow::Result<bool> {
    let keys = clipboard.connection.query_keymap()?.reply()?.keys;
    Ok(keyboard.held_modifiers(&keys) & !keyboard.lock_modifiers() != 0)
}

fn authorize_paste<C>(
    request: &Insertion,
    clipboard: &mut C,
    external_target: impl FnOnce(&C) -> bool,
    modifiers_held: impl FnOnce(&C) -> anyhow::Result<bool>,
    holds_text: impl FnOnce(&mut C) -> anyhow::Result<bool>,
) -> anyhow::Result<ControlFlow<Inserted>> {
    let external = external_target(clipboard);
    let released = !modifiers_held(clipboard)?;
    let owned = holds_text(clipboard);
    if let Some(outcome) =
        Delivery::Paste.preflight(external && !request.own_focus.held(), released)
    {
        return Ok(ControlFlow::Break(outcome));
    }
    if !owned? {
        return Ok(ControlFlow::Break(CLIPBOARD_CHANGED));
    }
    Ok(if request.permit.commit() {
        ControlFlow::Continue(())
    } else {
        ControlFlow::Break(Inserted::Cancelled)
    })
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
