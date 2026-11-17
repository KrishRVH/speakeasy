use std::{collections::VecDeque, panic};

use anyhow::Context as _;

use super::{
    CLIPBOARD_CHANGED, CLIPBOARD_CHANGED_BEFORE_PASTE, NativeInsert, Worker, insert, settle,
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

struct Fake {
    steps: Vec<Step>,
    clipboard: u8,
    changes_clipboard_at: Option<Step>,
    revokes_at: Option<(Step, InsertPermit)>,
    holds: bool,
    external_focus: bool,
    releases: VecDeque<bool>,
    restored: Option<async_channel::Sender<u8>>,
    targets: VecDeque<u8>,
    return_fails: bool,
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
            releases: VecDeque::new(),
            restored: None,
            targets: VecDeque::new(),
            return_fails: false,
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
        Some(self.clipboard)
    }

    fn write(&mut self, (): (), _: &str) -> anyhow::Result<()> {
        self.reach(Step::Write);
        self.clipboard = WRITTEN;
        Ok(())
    }

    fn restore(&mut self, contents: u8, mark: &u8) -> anyhow::Result<()> {
        self.reach(Step::Restore);
        if *mark == self.clipboard {
            self.clipboard = contents;
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
        Ok(self.holds && *mark == self.clipboard)
    }

    fn paste_keys(&mut self) -> anyhow::Result<Keys> {
        self.reach(Step::PasteKeys);
        Ok(Keys::Insert(Delivery::Paste))
    }

    fn text_keys(&mut self, _: &str) -> anyhow::Result<Keys> {
        self.reach(Step::TextKeys);
        Ok(Keys::Insert(Delivery::Direct))
    }

    fn return_keys(&mut self) -> anyhow::Result<Keys> {
        anyhow::ensure!(!self.return_fails, "Fixture Return failure");
        Ok(Keys::Return)
    }

    fn target(&mut self) -> Option<u8> {
        Some(self.targets.pop_front().unwrap_or(1))
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
            Step::Mark,
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
                settle(native, request)
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
