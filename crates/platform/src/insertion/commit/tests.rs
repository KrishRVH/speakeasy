use std::{collections::VecDeque, panic};

use anyhow::Context as _;

use super::{Continuation, NativeInsert, Worker, insert, settle};
use crate::{
    InputSender, InsertPermit, PATIENCE,
    insertion::{
        Delivery, Enter, Focus, FocusReport, INSERTION_STOPPED, Inserted, OwnFocus,
        tests::{DELIVERIES, RELEASED},
    },
};

mod delivery;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Copy,
    PasteKeys,
    TextKeys,
    Focus,
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

struct Fake {
    steps: Vec<Step>,
    clipboard: u8,
    snapshot: bool,
    changes_clipboard_at: Option<Step>,
    revokes_at: Option<(Step, InsertPermit)>,
    focus: Focus,
    releases: VecDeque<bool>,
    restored: Option<async_channel::Sender<u8>>,
    targets: VecDeque<u8>,
    activity: u8,
    texts: Vec<String>,
    return_fails: bool,
    refuses: bool,
    fails: Vec<Step>,
}

impl Fake {
    const fn new() -> Self {
        Self {
            steps: Vec::new(),
            clipboard: 0,
            snapshot: true,
            changes_clipboard_at: None,
            revokes_at: None,
            focus: Focus::Elsewhere,
            releases: VecDeque::new(),
            restored: None,
            targets: VecDeque::new(),
            activity: 0,
            texts: Vec::new(),
            return_fails: false,
            refuses: false,
            fails: Vec::new(),
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

    fn fail_at(&self, step: Step) -> anyhow::Result<()> {
        anyhow::ensure!(!self.fails.contains(&step), "Fixture {step:?} failure");
        Ok(())
    }

    fn sent(&self) -> bool {
        self.steps.iter().any(|step| matches!(step, Step::Send(_)))
    }
}

impl NativeInsert for Fake {
    type Activity = u8;
    type Contents = u8;
    type Keys = Keys;
    type Mark = u8;
    type Target = u8;

    fn copy(&mut self, text: &str) -> anyhow::Result<(u8, Option<u8>)> {
        let saved = self.snapshot.then_some(self.clipboard);
        self.reach(Step::Copy);
        self.fail_at(Step::Copy)?;
        self.texts.push(text.to_owned());
        self.clipboard = WRITTEN;
        Ok((WRITTEN, saved))
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

    fn paste_keys(&mut self) -> anyhow::Result<Keys> {
        self.reach(Step::PasteKeys);
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

    fn focus(&mut self) -> Focus {
        self.reach(Step::Focus);
        self.focus
    }

    fn shortcut_released(&mut self) -> bool {
        self.reach(Step::ShortcutReleased);
        self.releases.pop_front().unwrap_or(RELEASED)
    }

    fn refused(&mut self, _: Delivery) -> Option<Inserted> {
        self.refuses.then_some(REFUSED)
    }

    fn send(&mut self, keys: Keys) -> anyhow::Result<Inserted> {
        self.reach(Step::Send(keys));
        Ok(Inserted::Sent)
    }
}

const REFUSED: Inserted = Inserted::Unavailable("Fixture refusal");

fn unfocused() -> OwnFocus {
    FocusReport::default().subscribe()
}

fn permit() -> anyhow::Result<InsertPermit> {
    let (events, _receiver) = async_channel::bounded(1);
    InputSender::new(events).begin().context("Recording")
}

#[test]
fn paste_copies_then_checks_focus_and_sends_once() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake::new();
    assert_eq!(
        insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?.0,
        Inserted::Sent
    );
    assert_eq!(
        native.steps,
        [
            Step::PasteKeys,
            Step::Copy,
            Step::Focus,
            Step::ShortcutReleased,
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
        focus: Focus::Own,
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

#[test]
fn a_refused_delivery_sends_nothing_and_leaves_a_paste_on_the_clipboard() -> anyhow::Result<()> {
    for delivery in DELIVERIES {
        let mut native = Fake {
            refuses: true,
            ..Fake::new()
        };
        let (outcome, saved) = insert(&mut native, "text", &permit()?, delivery, &unfocused())?;
        assert_eq!(outcome, REFUSED, "{delivery:?}");
        assert!(!native.sent(), "{delivery:?}: a refused delivery was sent");
        assert!(
            saved.is_none(),
            "{delivery:?}: unsent text would be replaced"
        );
        assert_eq!(
            native.clipboard == WRITTEN,
            delivery == Delivery::Paste,
            "{delivery:?}: the dictation isn't left to paste by hand"
        );
    }
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
            Step::Focus,
            Step::ShortcutReleased,
            Step::Send(Keys::Insert(Delivery::Direct)),
        ]
    );
    assert!(!permit.commit(), "Direct input left its authority unspent");
    Ok(())
}

#[test]
fn escape_before_the_paste_puts_back_only_a_clipboard_the_dictation_still_holds()
-> anyhow::Result<()> {
    for (changed, snapshot, expected) in [
        (false, true, 1),
        (false, false, 0),
        (true, true, WRITTEN + 1),
    ] {
        let permit = permit()?;
        let mut native = Fake {
            clipboard: 1,
            snapshot,
            changes_clipboard_at: changed.then_some(Step::ShortcutReleased),
            revokes_at: Some((Step::ShortcutReleased, permit.clone())),
            ..Fake::new()
        };
        let (outcome, saved) = insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?;
        assert_eq!(outcome, Inserted::Cancelled);
        assert!(saved.is_none() && !native.sent());
        assert_eq!(
            native.clipboard, expected,
            "changed: {changed}, snapshot: {snapshot}"
        );
    }
    Ok(())
}

#[test]
fn escape_reports_cancelled_even_when_the_copy_or_restore_fails() -> anyhow::Result<()> {
    for failed in [Step::Copy, Step::Restore] {
        let permit = permit()?;
        let mut native = Fake {
            clipboard: 1,
            revokes_at: Some((Step::Copy, permit.clone())),
            fails: vec![failed],
            ..Fake::new()
        };
        let (outcome, saved) = insert(&mut native, "text", &permit, Delivery::Paste, &unfocused())?;
        assert_eq!(outcome, Inserted::Cancelled, "{failed:?}");
        assert!(saved.is_none() && !native.sent());
    }
    Ok(())
}

#[test]
fn a_failed_copy_sends_nothing() -> anyhow::Result<()> {
    let mut native = Fake {
        fails: vec![Step::Copy],
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
    .context("A failed copy reported an outcome")?;
    assert_eq!(error.to_string(), "Fixture Copy failure");
    assert!(!native.sent());
    Ok(())
}

#[test]
fn own_focus_leaves_the_text_copied_without_spending_authority() -> anyhow::Result<()> {
    let permit = permit()?;
    let mut native = Fake {
        focus: Focus::Own,
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
            delivery.check_focus(Focus::Own),
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
