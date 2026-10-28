//! The insertion lane every platform shares, delivery choices and their outcomes, and the checks
//! every native adapter runs before it touches the target.

#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod commit;

use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail};
use async_channel::{Receiver, Sender};

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) use self::commit::{NativeInsert, Worker};
use crate::InsertPermit;

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
#[derive(Clone, Debug)]
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

/// How dictated text reaches the focused editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Places the text on the clipboard and sends the paste shortcut.
    Paste,
    /// Synthesizes the text as key input, leaving the clipboard untouched.
    Direct,
}

impl Delivery {
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
}

/// Submission outcome; native input cannot prove that the target accepted text.
#[derive(Debug, PartialEq, Eq)]
pub enum Inserted {
    /// Native input was submitted once.
    Sent,
    /// Text remains on the clipboard with a manual-paste remedy.
    Copied(&'static str),
    /// Insertion authority was revoked before submission.
    Cancelled,
    /// Insertion is unavailable or incomplete; the message explains how to recover.
    Unavailable(&'static str),
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

/// Whether an arboard clipboard read found `text`. An image or files another app copied hold no
/// text to read; that is a change, not a fault.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn holds_text(read: Result<String, arboard::Error>, text: &str) -> anyhow::Result<bool> {
    match read {
        Ok(current) => Ok(current == text),
        Err(arboard::Error::ContentNotAvailable) => Ok(false),
        Err(error) => Err(error.into()),
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
