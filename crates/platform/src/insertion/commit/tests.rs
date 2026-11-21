use std::{collections::VecDeque, panic};

use anyhow::Context as _;

use super::{
    CLIPBOARD_CHANGED, CLIPBOARD_CHANGED_BEFORE_PASTE, Continuation, NativeInsert, Worker, insert,
    settle,
};
use crate::{
    InputSender, InsertPermit, PATIENCE,
    insertion::{
        Delivery, Enter, FocusReport, INSERTION_STOPPED, Inserted, OwnFocus,
        tests::{DELIVERIES, EXTERNAL_FOCUS, OWN_FOCUS, RELEASED},
    },
};

mod delivery;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Acquire,
    Save,
    Write,
    Mark,
    Holds,
    PasteKeys,
    TextKeys,
    ExternalFocus,
    ShortcutReleased,
    Send(Keys),
    Restore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keys {
    Insert(Delivery),
    Return,
}

const WRITTEN: u8 = 200;

#[derive(Clone, Copy, Debug)]
enum WriteFailure {
    BeforeClearing,
    Partway,
}

#[derive(Clone, Copy, Debug)]
enum SnapshotAccess {
    Available,
    Unavailable,
}

struct Fake {
    steps: Vec<Step>,
    clipboard: u8,
    snapshot: SnapshotAccess,
    changes_clipboard_at: Option<Step>,
    revokes_at: Option<(Step, InsertPermit)>,
    holds: bool,
    external_focus: bool,
    releases: VecDeque<bool>,
    restored: Option<async_channel::Sender<u8>>,
    targets: VecDeque<u8>,
    activity: u8,
    texts: Vec<String>,
    return_fails: bool,
    fails: Vec<Step>,
    write_fails: Option<WriteFailure>,
}

impl Fake {
    const fn new() -> Self {
        Self {
            steps: Vec::new(),
            clipboard: 0,
            snapshot: SnapshotAccess::Available,
            changes_clipboard_at: None,
            revokes_at: None,
            holds: true,
            external_focus: EXTERNAL_FOCUS,
            releases: VecDeque::new(),
            restored: None,
            targets: VecDeque::new(),
            activity: 0,
            texts: Vec::new(),
            return_fails: false,
            fails: Vec::new(),
            write_fails: None,
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

    fn fail_at(&self, step: Step) -> anyhow::Result<()> {
        anyhow::ensure!(!self.fails.contains(&step), "Fixture {step:?} failure");
        Ok(())
    }
}

impl NativeInsert for Fake {
    type Activity = u8;
    type Contents = u8;
    type Keys = Keys;
    type Lease = ();
    type Mark = u8;
    type Target = u8;

    fn acquire(&mut self) -> anyhow::Result<()> {
        self.reach(Step::Acquire);
        Ok(())
    }

    fn save(&mut self, (): &()) -> Option<u8> {
        self.reach(Step::Save);
        matches!(self.snapshot, SnapshotAccess::Available).then_some(self.clipboard)
    }

    fn write(&mut self, (): (), text: &str, saved: Option<&u8>) -> anyhow::Result<u8> {
        self.reach(Step::Write);
        self.texts.push(text.to_owned());
        match self.write_fails {
            None => {
                self.clipboard = WRITTEN;
                Ok(WRITTEN)
            },
            Some(failure) => {
                if matches!(failure, WriteFailure::Partway) {
                    self.clipboard = saved.copied().unwrap_or_default();
                }
                anyhow::bail!("Fixture write failure")
            },
        }
    }

    fn restore(&mut self, contents: Option<u8>, mark: &u8) -> anyhow::Result<()> {
        self.reach(Step::Restore);
        self.fail_at(Step::Restore)?;
        if *mark == self.clipboard {
            self.clipboard = contents.unwrap_or_default();
        }
        if let Some(restored) = &self.restored {
            restored.try_send(self.clipboard)?;
        }
        Ok(())
    }

    fn mark(&mut self) -> u8 {
        self.reach(Step::Mark);
        self.clipboard
    }

    fn holds(&mut self, _: &str, mark: &u8) -> anyhow::Result<bool> {
        self.reach(Step::Holds);
        self.fail_at(Step::Holds)?;
        Ok(self.holds && *mark == self.clipboard)
    }

    fn paste_keys(&mut self) -> anyhow::Result<Keys> {
        self.reach(Step::PasteKeys);
        self.fail_at(Step::PasteKeys)?;
        Ok(Keys::Insert(Delivery::Paste))
    }

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Keys> {
        self.reach(Step::TextKeys);
        self.texts.push(text.to_owned());
        Ok(Keys::Insert(Delivery::Direct))
    }

    fn return_keys(&mut self) -> anyhow::Result<Keys> {
        anyhow::ensure!(!self.return_fails, "Fixture Return failure");
        Ok(Keys::Return)
    }

    fn target(&mut self) -> Option<u8> {
        Some(self.targets.pop_front().unwrap_or(1))
    }

    fn activity(&mut self) -> Option<u8> {
        Some(self.activity)
    }

    fn external_focus(&mut self) -> bool {
        self.reach(Step::ExternalFocus);
        self.external_focus
    }

    fn shortcut_released(&mut self) -> bool {
        self.reach(Step::ShortcutReleased);
        self.releases.pop_front().unwrap_or(RELEASED)
    }

    fn send(&mut self, keys: Keys) -> anyhow::Result<Inserted> {
        self.reach(Step::Send(keys));
        Ok(Inserted::Sent)
    }
}

fn unfocused() -> OwnFocus {
    FocusReport::default().subscribe()
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
        insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?.0,
        Inserted::Sent
    );
    assert_eq!(
        native.steps,
        [
            Step::Acquire,
            Step::Save,
            Step::Write,
            Step::Holds,
            Step::PasteKeys,
            Step::ExternalFocus,
            Step::ShortcutReleased,
            Step::Mark,
            Step::Send(Keys::Insert(Delivery::Paste)),
        ]
    );
    assert!(!permit.commit(), "The paste left its authority unspent");
    Ok(())
}

#[test]
fn only_a_sent_paste_hands_back_the_clipboard_it_replaced() -> anyhow::Result<()> {
    let mut native = Fake::new();
    let (_, saved) = insert(
        &mut native,
        "text",
        &permit()?,
        Delivery::Paste,
        &unfocused(),
    )?;
    let saved = saved.context("A sent paste forgot the clipboard it replaced")?;
    assert_eq!((saved.contents, saved.mark), (0, WRITTEN));

    let mut native = Fake {
        external_focus: OWN_FOCUS,
        ..Fake::new()
    };
    let (_, kept) = insert(
        &mut native,
        "text",
        &permit()?,
        Delivery::Paste,
        &unfocused(),
    )?;
    assert!(
        kept.is_none(),
        "Text left to paste by hand would be replaced"
    );

    let mut native = Fake::new();
    let (_, typed) = insert(
        &mut native,
        "text",
        &permit()?,
        Delivery::Direct,
        &unfocused(),
    )?;
    assert!(typed.is_none());
    Ok(())
}

#[tokio::test]
async fn the_clipboard_returns_after_the_reply_unless_it_changed_again() -> anyhow::Result<()> {
    for changed in [false, true] {
        let (events, _receiver) = async_channel::bounded(1);
        let (restored, restoring) = async_channel::bounded(1);
        let (_worker, inserter) =
            Worker::start(InputSender::new(events), unfocused(), move |request| {
                let native = Fake {
                    clipboard: 7,
                    changes_clipboard_at: changed.then_some(Step::Restore),
                    restored: Some(restored.clone()),
                    ..Fake::new()
                };
                settle(native, request, &mut Continuation::default())
            })?;
        let sent = inserter
            .insert("text".into(), permit()?, Delivery::Paste, Enter::Withheld)
            .await?;
        assert_eq!(sent, Inserted::Sent);
        assert!(
            restoring.try_recv().is_err(),
            "The reply waited for the restore"
        );
        let clipboard = tokio::time::timeout(PATIENCE, restoring.recv()).await??;
        assert_eq!(clipboard == 7, !changed, "changed: {changed}");
    }
    Ok(())
}

#[test]
fn direct_input_never_touches_the_clipboard() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake::new();
    assert_eq!(
        insert(&mut native, "text", &permit, Delivery::Direct, &unfocused())?.0,
        Inserted::Sent
    );
    assert_eq!(
        native.steps,
        [
            Step::TextKeys,
            Step::ExternalFocus,
            Step::ShortcutReleased,
            Step::Send(Keys::Insert(Delivery::Direct)),
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
        insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?.0,
        Inserted::Cancelled
    );
    assert_eq!(native.steps, [Step::Acquire]);
    Ok(())
}

#[test]
fn escape_during_a_delayed_clipboard_snapshot_prevents_the_write() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake {
        revokes_at: Some((Step::Save, permit.clone())),
        ..Fake::new()
    };
    assert_eq!(
        insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?.0,
        Inserted::Cancelled
    );
    assert_eq!(native.steps, [Step::Acquire, Step::Save]);
    Ok(())
}

#[test]
fn cancellation_restores_the_snapshot_only_while_the_dictation_owns_the_clipboard()
-> anyhow::Result<()> {
    for (changed, snapshot, expected) in [
        (false, SnapshotAccess::Available, 1),
        (false, SnapshotAccess::Unavailable, 0),
        (true, SnapshotAccess::Available, WRITTEN + 1),
        (true, SnapshotAccess::Unavailable, WRITTEN + 1),
    ] {
        let permit = permit()?;
        let mut native = Fake {
            clipboard: 1,
            snapshot,
            changes_clipboard_at: changed.then_some(Step::ShortcutReleased),
            revokes_at: Some((Step::ShortcutReleased, permit.clone())),
            ..Fake::new()
        };
        let (outcome, ..) = insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?;
        assert_eq!(outcome, Inserted::Cancelled);
        assert!(!native.sent());
        assert_eq!(native.clipboard, expected);
    }
    Ok(())
}

#[test]
fn cancellation_after_the_write_reports_cancelled_even_when_the_restore_fails() -> anyhow::Result<()>
{
    for revoked_at in [Step::Write, Step::Holds, Step::ShortcutReleased, Step::Mark] {
        for fails in [vec![], vec![Step::Restore]] {
            let permit = permit()?;
            let mut native = Fake {
                clipboard: 1,
                revokes_at: Some((revoked_at, permit.clone())),
                fails,
                ..Fake::new()
            };
            let (outcome, saved) =
                insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())
                    .with_context(|| format!("Escape at {revoked_at:?} failed"))?;
            assert_eq!(outcome, Inserted::Cancelled, "Escape at {revoked_at:?}");
            assert!(saved.is_none() && !native.sent());
            assert!(
                native.steps.contains(&Step::Restore),
                "Escape at {revoked_at:?} left the cancelled text on the clipboard"
            );
            assert_eq!(native.clipboard == 1, native.fails.is_empty());
        }
    }
    Ok(())
}

#[test]
fn a_failed_write_restores_nothing_after_releasing_its_lease() -> anyhow::Result<()> {
    for snapshot in [SnapshotAccess::Available, SnapshotAccess::Unavailable] {
        for failure in [WriteFailure::BeforeClearing, WriteFailure::Partway] {
            let mut native = Fake {
                clipboard: 1,
                snapshot,
                changes_clipboard_at: Some(Step::Mark),
                write_fails: Some(failure),
                ..Fake::new()
            };
            let error = insert(
                &mut native,
                "text",
                &permit()?,
                Delivery::Paste,
                &unfocused(),
            )
            .err()
            .context("A failed write reported an outcome")?;
            assert_eq!(error.to_string(), "Fixture write failure");
            assert_eq!(
                native.steps,
                [Step::Acquire, Step::Save, Step::Write],
                "{failure:?} with {snapshot:?} reached past the failed write"
            );
            let kept = match (failure, snapshot) {
                (WriteFailure::Partway, SnapshotAccess::Unavailable) => 0,
                _ => 1,
            };
            assert_eq!(native.clipboard, kept, "{failure:?} with {snapshot:?}");
        }
    }
    Ok(())
}

#[test]
fn cancellation_restores_only_a_clipboard_that_still_holds_the_dictation() -> anyhow::Result<()> {
    for holds in [true, false] {
        let permit = permit()?;
        let mut native = Fake {
            clipboard: 1,
            holds,
            revokes_at: Some((Step::Write, permit.clone())),
            ..Fake::new()
        };
        let (outcome, ..) = insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?;
        assert_eq!(outcome, Inserted::Cancelled);
        assert_eq!(
            native.steps.contains(&Step::Restore),
            holds,
            "Escape restored a clipboard whose dictation check said {holds}"
        );
        assert_eq!(native.clipboard, if holds { 1 } else { WRITTEN });
    }
    Ok(())
}

#[test]
fn a_failed_restore_keeps_the_error_that_stopped_the_paste() -> anyhow::Result<()> {
    for failed in [Step::Holds, Step::PasteKeys] {
        for fails in [vec![failed], vec![failed, Step::Restore]] {
            let mut native = Fake {
                clipboard: 1,
                fails,
                ..Fake::new()
            };
            let error = insert(
                &mut native,
                "text",
                &permit()?,
                Delivery::Paste,
                &unfocused(),
            )
            .err()
            .context("A paste that failed reported an outcome")?;
            assert_eq!(error.to_string(), format!("Fixture {failed:?} failure"));
            assert!(!native.sent());
            let verified = failed == Step::PasteKeys;
            assert_eq!(
                native.steps.contains(&Step::Restore),
                verified,
                "A paste that failed at {failed:?} restored a clipboard it had not verified"
            );
            assert_eq!(
                native.clipboard == 1,
                verified && !native.fails.contains(&Step::Restore),
                "A paste that failed at {failed:?} kept the wrong copy"
            );
        }
    }
    Ok(())
}

#[test]
fn a_cancelled_paste_reports_cancelled_even_when_its_native_call_fails() -> anyhow::Result<()> {
    for failed in [Step::Write, Step::Holds, Step::PasteKeys] {
        let permit = permit()?;
        let mut native = Fake {
            clipboard: 1,
            revokes_at: Some((failed, permit.clone())),
            write_fails: (failed == Step::Write).then_some(WriteFailure::Partway),
            fails: vec![failed],
            ..Fake::new()
        };
        let (outcome, saved) = insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())
            .with_context(|| format!("Escape during a failed {failed:?} reported the failure"))?;
        assert_eq!(outcome, Inserted::Cancelled, "{failed:?}");
        assert!(saved.is_none() && !native.sent());
        let kept = if failed == Step::Holds { WRITTEN } else { 1 };
        assert_eq!(
            native.clipboard, kept,
            "Escape during a failed {failed:?} left the wrong copy"
        );
    }
    Ok(())
}

#[test]
fn a_fallback_never_claims_the_text_is_copied_after_the_clipboard_changed() -> anyhow::Result<()> {
    for (external_focus, released, changed_at) in [
        (OWN_FOCUS, RELEASED, Step::ExternalFocus),
        (EXTERNAL_FOCUS, !RELEASED, Step::ShortcutReleased),
    ] {
        let permit = permit()?;
        let mut native = Fake {
            external_focus,
            releases: VecDeque::from([released]),
            changes_clipboard_at: Some(changed_at),
            ..Fake::new()
        };
        assert_eq!(
            insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?.0,
            CLIPBOARD_CHANGED_BEFORE_PASTE,
            "Another copy during the {changed_at:?} check"
        );
        assert!(!native.sent());
    }
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
        insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?.0,
        CLIPBOARD_CHANGED
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
        insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?.0,
        CLIPBOARD_CHANGED_BEFORE_PASTE
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
        insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?.0,
        Inserted::Copied(_)
    ));
    assert!(!native.sent());
    assert!(permit.commit(), "A copied result spent its authority");
    Ok(())
}

#[test]
fn a_focused_speakeasy_window_never_receives_input() -> anyhow::Result<()> {
    let report = FocusReport::default();
    report.settle(7, true);
    for delivery in DELIVERIES {
        let permit = permit()?;
        let mut native = Fake::new();
        assert_eq!(
            Some(insert(&mut native, "text", &permit, delivery, &report.subscribe())?.0),
            delivery.check_focus(OWN_FOCUS),
            "{delivery:?} input ignored Speakeasy's own focus"
        );
        assert!(!native.sent(), "{delivery:?} input reached Speakeasy");
        assert!(permit.commit(), "Refused input spent its authority");
    }
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
            insert(&mut native, "text", &permit, delivery, &unfocused())?.0,
            Inserted::Cancelled
        );
        assert!(!native.sent(), "{delivery:?} input outlived its revocation");
    }
    Ok(())
}

#[tokio::test]
async fn a_stopped_worker_exits_while_inserters_outlive_it() -> anyhow::Result<()> {
    let (events, _receiver) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let (worker, inserter) =
        Worker::start(input.clone(), unfocused(), |_| Ok(Inserted::Sent).into())?;
    worker.request_stop();
    tokio::time::timeout(PATIENCE, worker.stopped())
        .await
        .context("The insertion thread waited for every Inserter to drop")?;
    let permit = input.begin().context("Recording")?;
    let refused = inserter
        .insert("text".into(), permit, Delivery::Paste, Enter::Withheld)
        .await
        .err()
        .context("A stopped worker accepted a request")?;
    assert_eq!(refused.to_string(), "Desktop input is unavailable");
    Ok(())
}

#[tokio::test]
async fn a_panic_answers_a_fixed_message_and_the_next_request_is_served() -> anyhow::Result<()> {
    let (events, _receiver) = async_channel::bounded(1);
    let (_worker, inserter) = Worker::start(InputSender::new(events), unfocused(), |request| {
        if request.text == "panics" {
            panic::resume_unwind(Box::new("private payload"));
        }
        Ok(Inserted::Sent).into()
    })?;
    let failed = inserter
        .insert("panics".into(), permit()?, Delivery::Paste, Enter::Withheld)
        .await
        .err()
        .context("A panicking insertion reported an outcome")?;
    assert_eq!(format!("{failed:#}"), INSERTION_STOPPED);
    let next = tokio::time::timeout(
        PATIENCE,
        inserter.insert("text".into(), permit()?, Delivery::Paste, Enter::Withheld),
    )
    .await
    .context("The request after a panic was never answered")??;
    assert_eq!(next, Inserted::Sent);
    Ok(())
}
