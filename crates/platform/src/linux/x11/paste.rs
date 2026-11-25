use std::panic::{self, AssertUnwindSafe};

use anyhow::{Context as _, ensure};
use async_channel::Receiver;
use x11rb::{
    protocol::{
        xkb,
        xkb::ConnectionExt as _,
        xproto::{self, ConnectionExt as _},
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
    linux::{self, COPIED_FOR_MANUAL_PASTE, HostSession, NO_PASTE_BINDING, PasteChord, PasteKeys},
};

const SECOND_LAYOUT: Inserted = Inserted::Copied(
    "Text copied. Automatic paste needs your first keyboard layout; paste manually with the current layout.",
);

/// Serializes X11 insertion on one connection; setup failure or a panic ends the thread.
///
/// Serving on would reuse a connection in an unknown state, so the thread reports why and
/// abandons each queued request, which answers its caller.
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
                // `opened` drops only after a failed setup is reported, so that report precedes
                // the generic failure `ready` returns.
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
                    // Never the panic payload, which could quote the text.
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
        let keys = PasteKeys::of(self.paste);
        let held = keys.is_some()
            && wait_for_released_modifiers(&request.permit, MODIFIER_WAIT, || {
                modifiers_held(&self.clipboard, &self.keyboard)
            })?;
        if !request.permit.active() {
            return Ok(Inserted::Cancelled);
        }
        // `set_text` reads back through arboard, ordering its unawaited selection claim before
        // XTEST on `connection`. Checking the permit before `?` lets Escape outrank a failed copy.
        let copied = self.clipboard.set_text(&request.text, &request.permit);
        if !request.permit.active() || !copied? {
            return Ok(Inserted::Cancelled);
        }
        let Some(keys) = keys else {
            return Ok(settle_copy(&request.permit, COPIED_FOR_MANUAL_PASTE));
        };
        if held {
            return Ok(settle_copy(&request.permit, PASTE_AFTER_RELEASE));
        }
        let Some(chord) =
            PasteChord::resolve(keys, |symbol| self.keyboard.unshifted_keycode(symbol))
        else {
            return Ok(settle_copy(&request.permit, NO_PASTE_BINDING));
        };
        let external = self
            .clipboard
            .external_target(HostSession::X11)
            .unwrap_or(false);
        if !uses_first_layout(&self.clipboard.connection).unwrap_or(false) {
            return Ok(settle_copy(&request.permit, SECOND_LAYOUT));
        }
        // Read after those round trips: modifiers pressed meanwhile would join the synthetic chord.
        let released = !modifiers_held(&self.clipboard, &self.keyboard)?;
        if let Some(outcome) = Delivery::Paste.preflight(linux::focus(request, external), released)
        {
            return Ok(settle_copy(&request.permit, outcome));
        }
        if !request.permit.commit() {
            return Ok(Inserted::Cancelled);
        }
        Ok(if submit_paste(&self.clipboard.connection, chord).is_ok() {
            Inserted::Sent
        } else {
            Inserted::Copied(
                "Text copied. Automatic paste did not complete; check your editor before pasting manually.",
            )
        })
    }
}

fn settle_copy(permit: &InsertPermit, outcome: Inserted) -> Inserted {
    if permit.commit() {
        outcome
    } else {
        Inserted::Cancelled
    }
}

/// Whether the first layout group is active, the only one `unshifted_keycode` reads.
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

/// Queues the whole chord, releases even after a failed press, then synchronizes once.
///
/// A round trip per event would hold synthetic modifiers down. Once committed, the chord
/// completes even if a cancel arrives.
fn submit_paste(connection: &XCBConnection, chord: PasteChord<u8>) -> anyhow::Result<()> {
    let press = |kind, code| connection.xtest_fake_input(kind, code, 0, 0, 0, 0, 0);
    let mut cookies = Vec::with_capacity(6);
    let pressed = chord.presses().try_for_each(|code| {
        cookies.push(press(xproto::KEY_PRESS_EVENT, code)?);
        Ok::<(), x11rb::errors::ConnectionError>(())
    });
    let mut outcome = pressed.map_err(anyhow::Error::from);
    for code in chord.presses().rev() {
        match press(xproto::KEY_RELEASE_EVENT, code) {
            Ok(cookie) => cookies.push(cookie),
            Err(error) => outcome = outcome.and(Err(error.into())),
        }
    }
    connection.get_input_focus()?.reply()?;
    for cookie in cookies {
        let checked = cookie.check().map_err(anyhow::Error::from);
        outcome = outcome.and(checked);
    }
    outcome
}

#[cfg(test)]
mod tests;
