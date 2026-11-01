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

pub(crate) const MODIFIER_WAIT: Duration = Duration::from_millis(800);

const MODIFIER_POLL: Duration = Duration::from_millis(2);

const INSERTION_STOPPED: &str = "Text insertion stopped unexpectedly";

pub(crate) const PASTE_AFTER_RELEASE: Inserted =
    Inserted::Copied("Text copied. Release your shortcut keys and paste into your editor.");

#[derive(Clone, Debug)]
pub struct Inserter {
    requests: Sender<Insertion>,
}

impl Inserter {
    pub(crate) fn lane() -> (Self, Receiver<Insertion>) {
        let (requests, incoming) = async_channel::bounded(1);
        (Self { requests }, incoming)
    }

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

pub(crate) struct Insertion {
    pub(crate) text: String,
    pub(crate) permit: InsertPermit,
    pub(crate) delivery: Delivery,
    pub(crate) reply: Reply<anyhow::Result<Inserted>>,
}

#[cfg(target_os = "linux")]
pub(crate) fn abandon_queued(requests: &Receiver<Insertion>) {
    requests.close();
    while let Ok(request) = requests.try_recv() {
        drop(request);
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Paste,
    Direct,
}

impl Delivery {
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

    pub(crate) fn preflight(
        self,
        external_focus: bool,
        shortcut_released: bool,
    ) -> Option<Inserted> {
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

#[derive(Debug, PartialEq, Eq)]
pub enum Inserted {
    Sent,
    Copied(&'static str),
    Cancelled,
    Unavailable(&'static str),
}

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

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn holds_text(read: Result<String, arboard::Error>, text: &str) -> anyhow::Result<bool> {
    match read {
        Ok(current) => Ok(current == text),
        Err(arboard::Error::ContentNotAvailable) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

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
