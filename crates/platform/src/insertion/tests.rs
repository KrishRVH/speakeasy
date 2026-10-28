//! Drives the modifier wait, UTF-16 chunking, and how an arboard read reports a clipboard change;
//! no test touches the clipboard or sends input.

use anyhow::Context as _;

use super::{utf16_chunks, wait_for_released_modifiers};
use crate::InputSender;

#[test]
fn modifier_wait_ends_at_release_or_revocation() -> anyhow::Result<()> {
    let (events, _receiver) = async_channel::bounded(1);
    let permit = InputSender::new(events).begin().context("Recording")?;
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_clipboard_that_now_holds_no_text_reads_as_changed_not_failed() -> anyhow::Result<()> {
    assert!(super::holds_text(Ok("fixture".into()), "fixture")?);
    assert!(!super::holds_text(Ok("other".into()), "fixture")?);
    assert!(
        matches!(
            super::holds_text(Err(arboard::Error::ContentNotAvailable), "fixture"),
            Ok(false)
        ),
        "An image another app copied over the text failed the insertion"
    );
    assert!(
        super::holds_text(Err(arboard::Error::ClipboardOccupied), "fixture").is_err(),
        "A clipboard fault read as an ordinary change"
    );
    Ok(())
}
