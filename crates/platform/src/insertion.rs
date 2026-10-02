//! Delivery choices and the pre-commit checks the macOS adapter runs.

#[cfg(any(target_os = "macos", test))]
use std::{
    thread,
    time::{Duration, Instant},
};

#[cfg(any(target_os = "macos", test))]
use super::{InsertPermit, Inserted};

/// How long insertion waits for the user to let go of shortcut modifiers.
#[cfg(any(target_os = "macos", test))]
const MODIFIER_WAIT: Duration = Duration::from_millis(800);
/// How often insertion rechecks held modifiers while it waits. Text recognized early can be ready
/// while the stopping press is still down, so each poll's delay adds directly to visible latency.
#[cfg(any(target_os = "macos", test))]
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
    /// The adapter runs it as the last check before committing, after every slow preparation step, so
    /// the focus and keys it judges are current.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn preflight(
        self,
        external_focus: bool,
        shortcut_released: bool,
    ) -> Option<Inserted> {
        self.check_focus(external_focus)
            .or_else(|| self.check_shortcut(shortcut_released))
    }

    /// Returns the outcome that stops insertion while Speakeasy itself has focus.
    #[cfg(any(target_os = "macos", test))]
    fn check_focus(self, external_focus: bool) -> Option<Inserted> {
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

    #[cfg(any(target_os = "macos", test))]
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
#[cfg(any(target_os = "macos", test))]
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
mod tests {
    use anyhow::Context;

    use super::{Delivery, Inserted, utf16_chunks, wait_for_released_modifiers};
    use crate::InputSender;

    const DELIVERIES: [Delivery; 2] = [Delivery::Paste, Delivery::Direct];
    const OWN_FOCUS: bool = false;
    const EXTERNAL_FOCUS: bool = true;
    const HELD: bool = false;
    const RELEASED: bool = true;

    #[test]
    fn own_focus_outranks_a_held_shortcut() {
        for delivery in DELIVERIES {
            let own_focus = delivery.check_focus(OWN_FOCUS);
            assert!(own_focus.is_some());
            assert_eq!(delivery.preflight(OWN_FOCUS, HELD), own_focus);
            assert_eq!(delivery.preflight(OWN_FOCUS, RELEASED), own_focus);
            assert_ne!(delivery.preflight(EXTERNAL_FOCUS, HELD), own_focus);
        }
    }

    #[test]
    fn an_external_editor_with_released_keys_proceeds() {
        for delivery in DELIVERIES {
            assert_eq!(delivery.check_focus(EXTERNAL_FOCUS), None);
            assert_eq!(delivery.preflight(EXTERNAL_FOCUS, RELEASED), None);
        }
    }

    #[test]
    fn paste_leaves_text_copied_and_direct_input_leaves_the_clipboard_alone() {
        for (external_focus, shortcut_released) in [(OWN_FOCUS, RELEASED), (EXTERNAL_FOCUS, HELD)] {
            assert!(matches!(
                Delivery::Paste.preflight(external_focus, shortcut_released),
                Some(Inserted::Copied(_))
            ));
            assert!(matches!(
                Delivery::Direct.preflight(external_focus, shortcut_released),
                Some(Inserted::Unavailable(_))
            ));
        }
    }

    #[test]
    fn modifier_wait_ends_at_release_or_revocation() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        let mut held = [true, true, false].into_iter();
        let still_held = wait_for_released_modifiers(&permit, || {
            held.next().context("Queried modifiers after their release")
        })?;
        assert!(!still_held);
        permit.revoke();
        let mut queries = 0;
        let still_held = wait_for_released_modifiers(&permit, || {
            queries += 1;
            Ok(true)
        })?;
        assert_eq!(
            (still_held, queries),
            (true, 1),
            "A revoked recording kept waiting"
        );
        Ok(())
    }

    #[test]
    fn utf16_chunks_count_code_units_and_keep_surrogate_pairs_whole() {
        let emoji = "😀".repeat(9);
        assert_eq!(
            utf16_chunks(&emoji, 16).collect::<Vec<_>>(),
            ["😀".repeat(8), "😀".to_owned()]
        );
        let straddling = format!("{}😀b", "a".repeat(15));
        assert_eq!(
            utf16_chunks(&straddling, 16).collect::<Vec<_>>(),
            [format!("{}😀", "a".repeat(15)), "b".to_owned()]
        );
        assert_eq!(utf16_chunks("", 16).count(), 0);
    }
}
