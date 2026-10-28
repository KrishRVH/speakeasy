//! The commit protocol Windows and macOS insertion follow, and the thread that serves it.

use std::panic::{self, AssertUnwindSafe};

use anyhow::anyhow;
use async_channel::Receiver;

#[cfg(any(target_os = "windows", target_os = "macos"))]
use super::wait_for_released_modifiers;
use super::{Delivery, INSERTION_STOPPED, Inserted, Inserter, Insertion, PASTE_AFTER_RELEASE};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use crate::native;
use crate::{InputSender, InsertPermit, OwnedThread};

/// The Windows and macOS insertion thread, which serves an [`Inserter`]'s requests in order.
pub(crate) struct Worker {
    thread: OwnedThread,
    /// The thread's end of the lane. Closing it ends the thread once queued requests finish, while
    /// the session owner may still hold `Inserter` clones.
    requests: Receiver<Insertion>,
    input: InputSender,
}

impl Worker {
    /// Starts the insertion thread for recordings that `input` authorizes.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn start(input: InputSender) -> std::io::Result<(Self, Inserter)> {
        // The thread lives as long as the monitor, so each request drains what AppKit autoreleases.
        #[cfg(target_os = "macos")]
        let deliver = |request: &Insertion| objc2::rc::autoreleasepool(|_| deliver(request));
        Self::spawn(input, deliver)
    }

    /// Starts a thread that answers each request with `serve_one`.
    fn spawn(
        input: InputSender,
        serve_one: impl Fn(&Insertion) -> anyhow::Result<Inserted> + Send + 'static,
    ) -> std::io::Result<(Self, Inserter)> {
        let (inserter, requests) = Inserter::lane();
        let thread = OwnedThread::spawn("insertion", {
            let requests = requests.clone();
            move || serve(&requests, serve_one)
        })?;
        let worker = Self {
            thread,
            requests,
            input,
        };
        Ok((worker, inserter))
    }

    /// Revokes the current recording, so a pending insertion ends without input, and closes the
    /// lane without joining the thread.
    pub(crate) fn request_stop(&self) {
        self.input.cancel();
        self.requests.close();
    }

    /// Resolves once the thread has finished its last request.
    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        self.thread.exited()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Serves requests one at a time until the lane closes and drains.
///
/// A request whose work panics answers [`INSERTION_STOPPED`], and the thread serves on: each
/// request builds its native state afresh, while an ended thread would strand later requests on
/// the lane the [`Worker`] holds open.
fn serve(
    requests: &Receiver<Insertion>,
    serve_one: impl Fn(&Insertion) -> anyhow::Result<Inserted>,
) {
    while let Ok(request) = requests.recv_blocking() {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| serve_one(&request)))
            .unwrap_or_else(|_| Err(anyhow!(INSERTION_STOPPED)));
        request.reply.send(outcome);
    }
}

/// Waits for the shortcut's release, then inserts through the native adapter.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn deliver(request: &Insertion) -> anyhow::Result<Inserted> {
    wait_for_released_modifiers(&request.permit, || Ok(native::modifiers().any()))?;
    insert(
        &mut native::Insert,
        &request.text,
        &request.permit,
        request.delivery,
    )
}

impl Delivery {
    /// Returns the outcome that stops insertion before it touches the target: Speakeasy has focus,
    /// or shortcut keys are still held. Focus takes precedence.
    ///
    /// [`insert`] runs it after its last slow preparation step, so the focus and keys it judges are
    /// current.
    fn preflight(self, external_focus: bool, shortcut_released: bool) -> Option<Inserted> {
        self.check_focus(external_focus)
            .or_else(|| self.check_shortcut(shortcut_released))
    }

    fn check_shortcut(self, shortcut_released: bool) -> Option<Inserted> {
        if shortcut_released {
            return None;
        }
        Some(match self {
            Self::Paste => PASTE_AFTER_RELEASE,
            Self::Direct => {
                Inserted::Unavailable("Release the shortcut and try again. Clipboard preserved.")
            },
        })
    }
}

/// The clipboard no longer holds the text just written, so pasting would insert someone else's.
const CLIPBOARD_CHANGED: Inserted = Inserted::Unavailable(
    "The clipboard changed, so the paste was not sent and its new contents were kept. Dictate again.",
);
/// The clipboard changed while the paste waited for focus and keys.
const CLIPBOARD_CHANGED_BEFORE_PASTE: Inserted = Inserted::Unavailable(
    "The clipboard changed before pasting, and its new contents were kept. Dictate again.",
);

/// The native steps of one Windows or macOS insertion. Implementations perform each step and
/// decide nothing; [`insert`] owns their order.
pub(crate) trait NativeInsert {
    /// Identifies one clipboard state, so a later change is detectable.
    type Mark: PartialEq;
    /// Clipboard access, held from [`acquire`](Self::acquire) until [`write`](Self::write).
    type Lease;
    /// Input built in full before commit, so a failed build never leaves partial input.
    type Keys;

    /// Takes the clipboard; this may block while another application holds it.
    fn acquire(&mut self) -> anyhow::Result<Self::Lease>;
    /// Places `text` on the clipboard, marked to stay out of clipboard history and other devices,
    /// then releases the lease.
    fn write(&mut self, lease: Self::Lease, text: &str) -> anyhow::Result<()>;
    /// The clipboard's current state.
    fn mark(&mut self) -> Self::Mark;
    /// Whether the clipboard still holds `text`, unchanged since `mark`.
    fn holds(&mut self, text: &str, mark: &Self::Mark) -> anyhow::Result<bool>;
    /// Builds the paste shortcut.
    fn paste_keys(&mut self) -> anyhow::Result<Self::Keys>;
    /// Builds key input that types `text`.
    fn text_keys(&mut self, text: &str) -> anyhow::Result<Self::Keys>;
    /// Whether an application other than Speakeasy has focus.
    fn external_focus(&mut self) -> bool;
    /// Whether every modifier key is up.
    fn shortcut_released(&mut self) -> bool;
    /// Submits `keys`; runs only after the permit commits, so it never rechecks it.
    fn send(&mut self, keys: Self::Keys) -> anyhow::Result<Inserted>;
}

/// Delivers `text` through `native` under `permit`, committing at most once and only after every
/// check that a slow native step could have made stale; native input follows the commit exactly
/// once.
///
/// A paste writes the clipboard only while the recording still holds authority, and sends only if
/// the clipboard still holds that text when Speakeasy commits.
pub(crate) fn insert<N: NativeInsert>(
    native: &mut N,
    text: &str,
    permit: &InsertPermit,
    delivery: Delivery,
) -> anyhow::Result<Inserted> {
    if !permit.active() {
        return Ok(Inserted::Cancelled);
    }
    let keys = match delivery {
        Delivery::Paste => {
            let lease = native.acquire()?;
            // Escape may have revoked the recording while another application held the clipboard.
            if !permit.active() {
                return Ok(Inserted::Cancelled);
            }
            native.write(lease, text)?;
            let mark = native.mark();
            if !native.holds(text, &mark)? {
                return Ok(CLIPBOARD_CHANGED);
            }
            let keys = native.paste_keys()?;
            if let Some(outcome) =
                delivery.preflight(native.external_focus(), native.shortcut_released())
            {
                return Ok(outcome);
            }
            if native.mark() != mark {
                return Ok(CLIPBOARD_CHANGED_BEFORE_PASTE);
            }
            keys
        },
        Delivery::Direct => {
            let keys = native.text_keys(text)?;
            if let Some(outcome) =
                delivery.preflight(native.external_focus(), native.shortcut_released())
            {
                return Ok(outcome);
            }
            keys
        },
    };
    if !permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    native.send(keys)
}

#[cfg(test)]
mod tests;
