//! Delivery choices and the pre-commit checks every native adapter shares.

use std::{
    thread,
    time::{Duration, Instant},
};

use super::{InsertPermit, Inserted};

/// How long insertion waits for the user to let go of shortcut modifiers.
pub(crate) const MODIFIER_WAIT: Duration = Duration::from_millis(800);
/// How often insertion rechecks held modifiers while it waits.
const MODIFIER_POLL: Duration = Duration::from_millis(2);

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
    /// Adapters run it as the last check before committing, after every slow preparation step, so
    /// the focus and keys it judges are current.
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) fn preflight(
        self,
        external_focus: bool,
        shortcut_released: bool,
    ) -> Option<Inserted> {
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
            Self::Paste => Inserted::Copied(
                "Text copied. Release your shortcut keys and paste into your editor.",
            ),
            Self::Direct => {
                Inserted::Unavailable("Release the shortcut and try again. Clipboard preserved.")
            },
        })
    }
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
