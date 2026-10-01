//! Shared pre-commit eligibility. Target focus wins over held modifiers so every
//! adapter gives the same next action. Native ownership and commit stay local.
use super::Inserted;
#[derive(Clone, Copy)]
pub(super) enum Mode {
    Paste,
    Direct,
}
pub(super) fn preflight(external: bool, released: bool, mode: Mode) -> Option<Inserted> {
    match (external, released, mode) {
        (false, _, Mode::Paste) => Some(Inserted::Copied(
            "Text copied. Focus an editor outside Speakeasy and paste.",
        )),
        (false, _, Mode::Direct) => Some(Inserted::Unavailable(
            "Focus an editor outside Speakeasy, then dictate again. Clipboard preserved.",
        )),
        (_, false, Mode::Paste) => Some(Inserted::Copied(
            "Text copied. Release your shortcut keys and paste into your editor.",
        )),
        (_, false, Mode::Direct) => Some(Inserted::Unavailable(
            "Release the shortcut and try again. Clipboard preserved.",
        )),
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_focus_has_consistent_precedence_and_preserves_delivery_mode() {
        for released in [false, true] {
            assert!(
                matches!(preflight(false, released, Mode::Paste), Some(Inserted::Copied(message)) if message.contains("Focus an editor"))
            );
            assert!(
                matches!(preflight(false, released, Mode::Direct), Some(Inserted::Unavailable(message)) if message.contains("Focus an editor"))
            );
        }
        assert!(
            matches!(preflight(true, false, Mode::Paste), Some(Inserted::Copied(message)) if message.contains("Release"))
        );
        assert!(
            matches!(preflight(true, false, Mode::Direct), Some(Inserted::Unavailable(message)) if message.contains("Release"))
        );
        assert!(preflight(true, true, Mode::Paste).is_none());
        assert!(preflight(true, true, Mode::Direct).is_none());
    }
}
