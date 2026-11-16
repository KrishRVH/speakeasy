use super::*;
use crate::machine::fixture::Sample;

fn saved(shown: &'static str) -> Delivered<Sample> {
    Delivered::Published(Published::Saved {
        shown,
        saved: (),
        copied: Ok(()),
    })
}

#[test]
fn a_show_saved_after_its_session_was_abandoned_never_replaces_a_later_sessions() {
    let first = SessionId::FIRST;
    let second = first.next().unwrap();
    assert_eq!(saved_anyway(None, first, saved("first")), Some("first"));
    assert_eq!(
        saved_anyway(Some(first), second, saved("second")),
        Some("second")
    );
    assert_eq!(
        saved_anyway(Some(second), first, saved("first")),
        None,
        "An earlier session's Show replaced a later one"
    );
}
