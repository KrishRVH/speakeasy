//! Delivery choices, the pre-commit checks every native adapter shares, and the commit protocol
//! Windows and macOS insertion follow.

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
    /// [`insert`] runs it as the last check before committing, after every slow preparation step,
    /// so the focus and keys it judges are current.
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
            Self::Paste => Inserted::Copied(
                "Text copied. Release your shortcut keys and paste into your editor.",
            ),
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
