//! The insertion lane every platform shares, delivery choices, the pre-commit checks every native
//! adapter shares, and the commit protocol Windows and macOS insertion follow.

#[cfg(any(target_os = "windows", target_os = "macos", test))]
use std::panic::{self, AssertUnwindSafe};
use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail};
use async_channel::{Receiver, Sender};

#[cfg(any(target_os = "windows", target_os = "macos"))]
use super::native;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
use super::{InputSender, OwnedThread};
use super::{InsertPermit, Inserted};

/// How long insertion waits for the user to let go of shortcut modifiers.
pub(crate) const MODIFIER_WAIT: Duration = Duration::from_millis(800);
/// How often insertion rechecks held modifiers while it waits.
const MODIFIER_POLL: Duration = Duration::from_millis(2);
/// The answer to a request whose insertion ended without an outcome; never the panic payload,
/// which could quote the text.
const INSERTION_STOPPED: &str = "Text insertion stopped unexpectedly";
/// Shortcut keys stayed held, so the text waits on the clipboard for the user to paste.
pub(crate) const PASTE_AFTER_RELEASE: Inserted =
    Inserted::Copied("Text copied. Release your shortcut keys and paste into your editor.");

/// Sends insertion requests to the thread that owns native clipboard and input access.
///
/// That thread performs one insertion at a time, and each keeps its turn through its native work
/// even if the caller stops waiting, so a cancelled request never races a newer clipboard write.
#[derive(Clone)]
pub struct Inserter {
    requests: Sender<Insertion>,
}

impl Inserter {
    /// An inserter and the end its requests arrive on.
    pub(crate) fn lane() -> (Self, Receiver<Insertion>) {
        let (requests, incoming) = async_channel::bounded(1);
        (Self { requests }, incoming)
    }

    /// Submits text under the recording permit through the chosen delivery.
    ///
    /// # Errors
    /// Returns native insertion failures, or a fixed message once the request lane has closed or
    /// its thread ended without answering.
    pub async fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        delivery: Delivery,
    ) -> anyhow::Result<Inserted> {
        if !permit.active() {
            return Ok(Inserted::Cancelled);
        }
        let (reply, answer) = Reply::channel();
        let request = Insertion {
            text,
            permit,
            delivery,
            reply,
        };
        if self.requests.send(request).await.is_err() {
            bail!("Desktop input is unavailable");
        }
        answer
            .recv()
            .await
            .unwrap_or_else(|_| Err(anyhow!(INSERTION_STOPPED)))
    }
}

/// One request on an [`Inserter`]'s lane.
pub(crate) struct Insertion {
    pub(crate) text: String,
    pub(crate) permit: InsertPermit,
    pub(crate) delivery: Delivery,
    pub(crate) reply: Reply<anyhow::Result<Inserted>>,
}

/// Closes an [`Inserter`]'s lane and drops each request still queued on it, which answers its
/// caller with [`INSERTION_STOPPED`]. Losing every reader only closes a lane: its queued requests
/// live on with the caller's `Inserter` clones, so a thread that stops reading calls this or leaves
/// their callers waiting forever.
#[cfg(target_os = "linux")]
pub(crate) fn abandon_queued(requests: &Receiver<Insertion>) {
    requests.close();
    while let Ok(request) = requests.try_recv() {
        drop(request);
    }
}

/// The single answer to one request.
pub(crate) struct Reply<T>(Sender<T>);

impl<T> Reply<T> {
    pub(crate) fn channel() -> (Self, Receiver<T>) {
        let (sender, receiver) = async_channel::bounded(1);
        (Self(sender), receiver)
    }

    pub(crate) fn send(self, answer: T) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A requester that stopped waiting dropped its receiver; native work already checked its recording permit"
        )]
        let _ = self.0.try_send(answer);
    }
}

/// The Windows and macOS insertion thread, which serves an [`Inserter`]'s requests in order.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub(crate) struct Worker {
    thread: OwnedThread,
    /// The thread's end of the lane. Closing it ends the thread once queued requests finish, while
    /// the session owner may still hold `Inserter` clones.
    requests: Receiver<Insertion>,
    input: InputSender,
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl Worker {
    /// Starts the insertion thread for recordings that `input` authorizes.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn start(input: InputSender) -> std::io::Result<(Self, Inserter)> {
        // The thread lives as long as the monitor, so each request drains what AppKit autoreleases.
        #[cfg(target_os = "macos")]
        let deliver = |request: &Insertion| objc2::rc::autoreleasepool(|_| deliver(request));
        Self::serving(input, deliver)
    }

    /// Starts a thread that answers each request with `serve_one`.
    fn serving(
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
    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.thread.exited()
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
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
#[cfg(any(target_os = "windows", target_os = "macos", test))]
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

/// How dictated text reaches the focused editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Places the text on the clipboard and sends the paste shortcut.
    Paste,
    /// Synthesizes the text as key input, leaving the clipboard untouched.
    Direct,
}

impl Delivery {
    /// Returns the outcome that stops insertion before it touches the target: Speakeasy has focus,
    /// or shortcut keys are still held. Focus takes precedence.
    ///
    /// [`insert`] runs it after every slow preparation step, so the focus and keys it judges are
    /// current.
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    fn preflight(self, external_focus: bool, shortcut_released: bool) -> Option<Inserted> {
        self.check_focus(external_focus)
            .or_else(|| self.check_shortcut(shortcut_released))
    }

    /// Returns the outcome that stops insertion while Speakeasy itself has focus.
    pub(crate) fn check_focus(self, external_focus: bool) -> Option<Inserted> {
        if external_focus {
            return None;
        }
        Some(match self {
            Self::Paste => {
                Inserted::Copied("Text copied. Focus an editor outside Speakeasy and paste.")
            },
            Self::Direct => Inserted::Unavailable(
                "Focus an editor outside Speakeasy, then dictate again. Clipboard preserved.",
            ),
        })
    }

    #[cfg(any(target_os = "windows", target_os = "macos", test))]
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
#[cfg(any(target_os = "windows", target_os = "macos", test))]
const CLIPBOARD_CHANGED: &str =
    "Clipboard changed. New contents were preserved; paste was not sent.";
/// The clipboard changed while the paste waited for focus and keys.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
const CLIPBOARD_CHANGED_BEFORE_PASTE: &str =
    "Clipboard changed before paste. New contents were preserved.";

/// The native steps of one Windows or macOS insertion. Implementations perform each step and
/// decide nothing; [`insert`] owns their order.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub(crate) trait NativeInsert {
    /// Identifies one clipboard state, so a later change is detectable.
    type Mark: PartialEq;
    /// Clipboard access, held from [`acquire`](Self::acquire) until [`write`](Self::write).
    type Lease;
    /// Input built in full before commit, so a failed build never leaves partial input.
    type Keys;

    /// Takes the clipboard; this may block while another application holds it.
    fn acquire(&mut self) -> anyhow::Result<Self::Lease>;
    /// Places `text` on the clipboard, then releases the lease.
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
#[cfg(any(target_os = "windows", target_os = "macos", test))]
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
                return Ok(Inserted::Unavailable(CLIPBOARD_CHANGED));
            }
            let keys = native.paste_keys()?;
            if let Some(outcome) =
                delivery.preflight(native.external_focus(), native.shortcut_released())
            {
                return Ok(outcome);
            }
            if native.mark() != mark {
                return Ok(Inserted::Unavailable(CLIPBOARD_CHANGED_BEFORE_PASTE));
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

/// Gives the user up to [`MODIFIER_WAIT`] to let go of the shortcut, unless the recording is
/// revoked first; returns whether `held` still reported shortcut keys down.
pub(crate) fn wait_for_released_modifiers(
    permit: &InsertPermit,
    mut held: impl FnMut() -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let started = Instant::now();
    loop {
        let down = held()?;
        if !down || started.elapsed() >= MODIFIER_WAIT || !permit.active() {
            return Ok(down);
        }
        thread::sleep(MODIFIER_POLL);
    }
}

/// Splits `text` into runs of whole characters, closing each run once it holds at least `units`
/// UTF-16 code units, so a surrogate pair never straddles two runs.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn utf16_chunks(text: &str, units: usize) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut counted = 0_usize;
        let end = rest
            .char_indices()
            .find_map(|(start, character)| {
                counted = counted.saturating_add(character.len_utf16());
                (counted >= units).then(|| start.saturating_add(character.len_utf8()))
            })
            .unwrap_or(rest.len());
        let (chunk, tail) = rest.split_at_checked(end)?;
        rest = tail;
        Some(chunk)
    })
}

#[cfg(test)]
mod tests;
