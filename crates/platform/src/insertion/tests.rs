use anyhow::Context;

use super::{
    CLIPBOARD_CHANGED, CLIPBOARD_CHANGED_BEFORE_PASTE, Delivery, Inserted, NativeInsert, insert,
    utf16_chunks, wait_for_released_modifiers,
};
use crate::{InputSender, InsertPermit};

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

/// A native step of one insertion, as [`Fake`] records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Acquire,
    Write,
    Mark,
    Holds,
    PasteKeys,
    TextKeys,
    ExternalFocus,
    ShortcutReleased,
    /// Submitted the keys built for this delivery.
    Send(Delivery),
}

/// Records the steps [`insert`] takes, and revokes the recording or changes the clipboard when
/// it reaches a chosen one.
struct Fake {
    steps: Vec<Step>,
    clipboard: u8,
    changes_clipboard_at: Option<Step>,
    revokes_at: Option<(Step, InsertPermit)>,
    holds: bool,
    external_focus: bool,
}

impl Fake {
    const fn new() -> Self {
        Self {
            steps: Vec::new(),
            clipboard: 0,
            changes_clipboard_at: None,
            revokes_at: None,
            holds: true,
            external_focus: EXTERNAL_FOCUS,
        }
    }

    fn reach(&mut self, step: Step) {
        self.steps.push(step);
        if self.changes_clipboard_at == Some(step) {
            self.clipboard = self.clipboard.wrapping_add(1);
        }
        if let Some((at, permit)) = &self.revokes_at
            && *at == step
        {
            permit.revoke();
        }
    }

    fn sent(&self) -> bool {
        self.steps.iter().any(|step| matches!(step, Step::Send(_)))
    }
}

impl NativeInsert for Fake {
    type Keys = Delivery;
    type Lease = ();
    type Mark = u8;

    fn acquire(&mut self) -> anyhow::Result<()> {
        self.reach(Step::Acquire);
        Ok(())
    }

    fn write(&mut self, (): (), _: &str) -> anyhow::Result<()> {
        self.reach(Step::Write);
        Ok(())
    }

    fn mark(&mut self) -> u8 {
        self.reach(Step::Mark);
        self.clipboard
    }

    fn holds(&mut self, _: &str, mark: &u8) -> anyhow::Result<bool> {
        self.reach(Step::Holds);
        Ok(self.holds && *mark == self.clipboard)
    }

    fn paste_keys(&mut self) -> anyhow::Result<Delivery> {
        self.reach(Step::PasteKeys);
        Ok(Delivery::Paste)
    }

    fn text_keys(&mut self, _: &str) -> anyhow::Result<Delivery> {
        self.reach(Step::TextKeys);
        Ok(Delivery::Direct)
    }

    fn external_focus(&mut self) -> bool {
        self.reach(Step::ExternalFocus);
        self.external_focus
    }

    fn shortcut_released(&mut self) -> bool {
        self.reach(Step::ShortcutReleased);
        RELEASED
    }

    fn send(&mut self, keys: Delivery) -> anyhow::Result<Inserted> {
        self.reach(Step::Send(keys));
        Ok(Inserted::Sent)
    }
}

fn permit() -> anyhow::Result<InsertPermit> {
    let (events, _receiver) = async_channel::bounded(1);
    InputSender::new(events).begin().context("Recording")
}

#[test]
fn paste_checks_the_clipboard_again_after_focus_and_sends_once() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake::new();
    assert_eq!(
        insert(&mut native, "text", &permit, Delivery::Paste)?,
        Inserted::Sent
    );
    assert_eq!(
        native.steps,
        [
            Step::Acquire,
            Step::Write,
            Step::Mark,
            Step::Holds,
            Step::PasteKeys,
            Step::ExternalFocus,
            Step::ShortcutReleased,
            Step::Mark,
            Step::Send(Delivery::Paste),
        ]
    );
    assert!(!permit.commit(), "The paste left its authority unspent");
    Ok(())
}

#[test]
fn direct_input_never_touches_the_clipboard() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake::new();
    assert_eq!(
        insert(&mut native, "text", &permit, Delivery::Direct)?,
        Inserted::Sent
    );
    assert_eq!(
        native.steps,
        [
            Step::TextKeys,
            Step::ExternalFocus,
            Step::ShortcutReleased,
            Step::Send(Delivery::Direct),
        ]
    );
    assert!(!permit.commit(), "Direct input left its authority unspent");
    Ok(())
}

#[test]
fn escape_while_another_app_holds_the_clipboard_prevents_the_write() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake {
        revokes_at: Some((Step::Acquire, permit.clone())),
        ..Fake::new()
    };
    assert_eq!(
        insert(&mut native, "text", &permit, Delivery::Paste)?,
        Inserted::Cancelled
    );
    assert_eq!(native.steps, [Step::Acquire]);
    Ok(())
}

#[test]
fn a_clipboard_that_lost_the_text_is_never_pasted() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake {
        holds: false,
        ..Fake::new()
    };
    assert_eq!(
        insert(&mut native, "text", &permit, Delivery::Paste)?,
        Inserted::Unavailable(CLIPBOARD_CHANGED)
    );
    assert!(!native.sent());
    assert!(permit.commit(), "A refused paste spent its authority");
    Ok(())
}

#[test]
fn a_clipboard_change_during_the_final_checks_is_never_pasted() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake {
        changes_clipboard_at: Some(Step::ExternalFocus),
        ..Fake::new()
    };
    assert_eq!(
        insert(&mut native, "text", &permit, Delivery::Paste)?,
        Inserted::Unavailable(CLIPBOARD_CHANGED_BEFORE_PASTE)
    );
    assert!(!native.sent());
    assert!(permit.commit(), "A refused paste spent its authority");
    Ok(())
}

#[test]
fn own_focus_leaves_the_text_copied_without_spending_authority() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake {
        external_focus: OWN_FOCUS,
        ..Fake::new()
    };
    assert!(matches!(
        insert(&mut native, "text", &permit, Delivery::Paste)?,
        Inserted::Copied(_)
    ));
    assert!(!native.sent());
    assert!(permit.commit(), "A copied result spent its authority");
    Ok(())
}

#[test]
fn escape_during_the_final_checks_prevents_input() -> anyhow::Result<()> {
    for delivery in DELIVERIES {
        let permit = permit()?;
        let mut native = Fake {
            revokes_at: Some((Step::ShortcutReleased, permit.clone())),
            ..Fake::new()
        };
        assert_eq!(
            insert(&mut native, "text", &permit, delivery)?,
            Inserted::Cancelled
        );
        assert!(!native.sent(), "{delivery:?} input outlived its revocation");
    }
    Ok(())
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

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[tokio::test]
async fn a_stopped_worker_exits_while_inserters_outlive_it() -> anyhow::Result<()> {
    let (events, _receiver) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let (worker, inserter) = super::Worker::start(input.clone())?;
    worker.request_stop();
    tokio::time::timeout(std::time::Duration::from_secs(5), worker.stopped())
        .await
        .context("The insertion thread waited for every Inserter to drop")?;
    let permit = input.begin().context("Recording")?;
    let refused = inserter
        .insert("text".into(), permit, Delivery::Paste)
        .await
        .err()
        .context("A stopped worker accepted a request")?;
    assert_eq!(refused.to_string(), "Desktop input is unavailable");
    Ok(())
}
