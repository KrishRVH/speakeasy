use std::sync::mpsc;

use anyhow::bail;
use futures_util::FutureExt as _;

use super::*;
use crate::{Input, PATIENCE};

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Enqueue { kind: u8, code: u8, accepted: bool },
    Synchronize,
    Check,
}

struct PasteTrace {
    result: Submission,
    enqueued: Vec<(u8, u8, bool)>,
    checks: usize,
}

fn trace_paste(
    shift: Option<u8>,
    fail_at: Option<usize>,
    reject_check: Option<usize>,
    synchronized: bool,
) -> PasteTrace {
    let (log, steps) = mpsc::channel();
    let (enqueue_log, synchronize_log) = (log.clone(), log.clone());
    let mut positions = 0..;
    let mut checked = 0..;
    let result = submit_paste(
        PasteChord {
            control: 1,
            shift,
            v: 3,
        },
        move |kind, code| {
            let accepted = positions.next() != fail_at;
            enqueue_log.send(Step::Enqueue {
                kind,
                code,
                accepted,
            })?;
            ensure!(accepted, "Synthetic enqueue failure");
            Ok(())
        },
        move || {
            synchronize_log.send(Step::Synchronize)?;
            ensure!(synchronized, "Synthetic connection failure");
            Ok(())
        },
        move |()| {
            log.send(Step::Check)?;
            ensure!(checked.next() != reject_check, "Synthetic server rejection");
            Ok(())
        },
    );
    let steps: Vec<_> = steps.try_iter().collect();
    let barrier = steps
        .iter()
        .position(|step| *step == Step::Synchronize)
        .expect("Paste never synchronized");
    let (before_barrier, from_barrier) = steps.split_at(barrier);
    let checks = &from_barrier[1..];
    assert!(
        checks.iter().all(|step| *step == Step::Check),
        "Enqueued after the synchronization barrier"
    );
    let enqueued = before_barrier
        .iter()
        .map(|step| match *step {
            Step::Enqueue {
                kind,
                code,
                accepted,
            } => (kind, code, accepted),
            Step::Synchronize | Step::Check => {
                panic!("Checked before the sequence was released")
            },
        })
        .collect();
    PasteTrace {
        result,
        enqueued,
        checks: checks.len(),
    }
}

#[test]
fn paste_checks_once_after_all_releases_and_preserves_error_cleanup() {
    let release = |code| (xproto::KEY_RELEASE_EVENT, code);
    for shift in [None, Some(2)] {
        let trace = trace_paste(shift, None, None, true);
        assert_eq!(trace.result, Submission::Sent);
        assert_eq!(trace.checks, if shift.is_some() { 6 } else { 4 });
        assert_eq!(
            trace.enqueued.last().map(|&(kind, code, _)| (kind, code)),
            Some(release(1))
        );
    }
    for fail_at in [None, Some(0), Some(2), Some(3)] {
        let trace = trace_paste(Some(2), fail_at, Some(0), true);
        assert_ne!(trace.result, Submission::Sent);
        let accepted = trace
            .enqueued
            .iter()
            .filter(|&&(.., accepted)| accepted)
            .count();
        assert_eq!(
            trace.checks, accepted,
            "An earlier rejection skipped later error checks"
        );
        let attempted: Vec<_> = trace
            .enqueued
            .iter()
            .map(|&(kind, code, _)| (kind, code))
            .collect();
        for code in [3, 2, 1] {
            assert_eq!(
                attempted
                    .iter()
                    .filter(|&&event| event == release(code))
                    .count(),
                1
            );
        }
        assert_eq!(
            attempted[attempted.len() - 3..],
            [release(3), release(2), release(1)]
        );
        if fail_at.is_none() {
            assert_eq!(attempted.len(), 6);
        }
    }
}

fn paste_request(permit: &InsertPermit, own_focus: crate::OwnFocus) -> Insertion {
    let (reply, _answer) = Reply::channel();
    Insertion {
        text: "fixture".into(),
        permit: permit.clone(),
        delivery: Delivery::Paste,
        enter: crate::Enter::Withheld,
        own_focus,
        reply,
    }
}

#[test]
fn a_paste_tells_failure_before_its_v_press_from_uncertain_delivery_after_it() {
    for (fail_at, reject_check, synchronized, expected) in [
        (Some(2), None, true, Submission::NotSent),
        (None, Some(2), true, Submission::NotSent),
        (Some(0), None, false, Submission::NotSent),
        (None, Some(3), true, Submission::Uncertain),
        (Some(3), None, true, Submission::Uncertain),
        (None, None, false, Submission::Uncertain),
    ] {
        assert_eq!(
            trace_paste(Some(2), fail_at, reject_check, synchronized).result,
            expected,
            "Enqueue failure at {fail_at:?}, rejected check {reject_check:?}, synchronized {synchronized}"
        );
    }
}

#[test]
fn focus_moving_to_speakeasy_during_a_clipboard_check_refuses_the_paste() -> anyhow::Result<()> {
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    for during_owner_check in [false, true] {
        let permit = input.begin().context("Recording")?;
        let report = crate::FocusReport::default();
        let request = paste_request(&permit, report.subscribe());
        let focus_during = |owner_check| {
            if owner_check == during_owner_check {
                report.settle(7, true);
            }
        };
        let refused = authorize_paste(
            &request,
            &mut (),
            |()| {
                focus_during(false);
                Ok(true)
            },
            |()| true,
            |()| true,
            |()| Ok(false),
            |()| {
                focus_during(true);
                Ok(true)
            },
        )?;
        assert_eq!(
            refused.break_value(),
            Delivery::Paste.check_focus(false),
            "Focus that reached Speakeasy during a clipboard check went unseen (owner check: {during_owner_check})"
        );
        assert!(permit.commit(), "A refused paste spent its authority");
    }
    Ok(())
}

#[test]
fn a_copy_made_as_focus_reaches_speakeasy_is_reported_as_a_changed_clipboard() -> anyhow::Result<()>
{
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let permit = input.begin().context("Recording")?;
    let report = crate::FocusReport::default();
    let request = paste_request(&permit, report.subscribe());
    let refused = authorize_paste(
        &request,
        &mut (),
        |()| Ok(true),
        |()| true,
        |()| true,
        |()| Ok(false),
        |()| {
            report.settle(7, true);
            Ok(false)
        },
    )?;
    assert_eq!(
        refused.break_value(),
        Some(CLIPBOARD_CHANGED),
        "A copy the owner check saw was offered for manual pasting"
    );
    assert!(permit.commit(), "A refused paste spent its authority");
    Ok(())
}

#[test]
fn a_modifier_or_layout_change_during_the_last_clipboard_check_refuses_the_paste()
-> anyhow::Result<()> {
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    for layout in [false, true] {
        let permit = input.begin().context("Recording")?;
        let request = paste_request(&permit, crate::FocusReport::default().subscribe());
        let mut desk = (false, false);
        let refused = authorize_paste(
            &request,
            &mut desk,
            |(pressed, switched)| {
                *pressed = !layout;
                *switched = layout;
                Ok(true)
            },
            |_| true,
            |&mut (_, switched)| !switched,
            |&mut (pressed, _)| Ok(pressed),
            |_| Ok(true),
        )?;
        assert_eq!(
            refused.break_value(),
            Some(if layout {
                SECOND_LAYOUT
            } else {
                PASTE_AFTER_RELEASE
            }),
            "A change during the clipboard check went unseen (layout: {layout})"
        );
        assert!(permit.commit(), "A refused paste spent its authority");
    }
    Ok(())
}

#[test]
fn a_copy_made_during_the_focus_layout_or_modifier_queries_refuses_the_paste() -> anyhow::Result<()>
{
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    for copied_during in ["focus", "layout", "modifiers"] {
        let permit = input.begin().context("Recording")?;
        let request = paste_request(&permit, crate::FocusReport::default().subscribe());
        let copy = |owner: &mut Window, query| {
            if query == copied_during {
                *owner = 9;
            }
        };
        let mut owner: Window = 7;
        let refused = authorize_paste(
            &request,
            &mut owner,
            |_| Ok(true),
            |owner| {
                copy(owner, "focus");
                true
            },
            |owner| {
                copy(owner, "layout");
                true
            },
            |owner| {
                copy(owner, "modifiers");
                Ok(false)
            },
            |owner| Ok(*owner == 7),
        )?;
        assert!(
            refused.break_value() == Some(CLIPBOARD_CHANGED),
            "A copy made during the {copied_during} query was pasted"
        );
        assert!(permit.commit(), "A refused paste spent its authority");
    }
    Ok(())
}

#[test]
fn a_failed_or_unreadable_clipboard_check_never_reports_a_copy() -> anyhow::Result<()> {
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let permit = input.begin().context("Recording")?;
    let report = crate::FocusReport::default();
    report.settle(7, true);
    let request = paste_request(&permit, report.subscribe());
    let changed = authorize_paste(
        &request,
        &mut (),
        |()| Ok(false),
        |()| false,
        |()| true,
        |()| Ok(true),
        |()| Ok(true),
    )?;
    assert_eq!(
        changed.break_value(),
        Some(CLIPBOARD_CHANGED),
        "Another clipboard item was left for manual paste"
    );
    let unread = authorize_paste(
        &request,
        &mut (),
        |()| bail!("Synthetic clipboard failure"),
        |()| false,
        |()| true,
        |()| Ok(true),
        |()| Ok(true),
    );
    assert!(
        unread.is_err(),
        "An unreadable clipboard was left for manual paste"
    );
    Ok(())
}

#[test]
fn escape_during_a_failing_paste_check_reports_cancellation() -> anyhow::Result<()> {
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let permit = input.begin().context("Recording")?;
    let request = paste_request(&permit, crate::FocusReport::default().subscribe());
    let checked = authorize_paste(
        &request,
        &mut (),
        |()| {
            permit.revoke();
            bail!("Synthetic owner loss")
        },
        |()| false,
        |()| true,
        |()| Ok(false),
        |()| Ok(true),
    );
    assert!(
        matches!(checked, Ok(ControlFlow::Break(Inserted::Cancelled))),
        "A paste cancelled during a failing clipboard check ended {checked:?}"
    );
    Ok(())
}

#[test]
fn a_copy_escaped_before_its_outcome_reports_cancellation_and_a_kept_one_commits()
-> anyhow::Result<()> {
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let kept = input.begin().context("Kept recording")?;
    assert_eq!(
        settle_copy(&kept, COPIED_FOR_MANUAL_PASTE),
        COPIED_FOR_MANUAL_PASTE
    );
    assert!(!kept.active(), "A kept copy left its recording cancellable");
    let escaped = input.begin().context("Escaped recording")?;
    escaped.revoke();
    for outcome in [
        COPIED_FOR_MANUAL_PASTE,
        PASTE_AFTER_RELEASE,
        CLIPBOARD_CHANGED,
    ] {
        assert_eq!(
            settle_copy(&escaped, outcome),
            Inserted::Cancelled,
            "A copy reported an outcome after Escape"
        );
    }
    Ok(())
}

#[tokio::test]
async fn readiness_waits_for_insertion_setup_and_follows_its_failure_report() -> anyhow::Result<()>
{
    let (events, received) = async_channel::bounded(4);
    let input = InputSender::new(events);
    let (inserter, requests) = crate::Inserter::lane(crate::FocusReport::default().subscribe());
    let (entered, opening) = async_channel::bounded(1);
    let (release, released) = mpsc::channel();
    let worker = InsertionWorker::spawn(
        input.clone(),
        requests,
        move || -> anyhow::Result<()> {
            entered.try_send(())?;
            released.recv_timeout(PATIENCE)?;
            bail!("Synthetic setup failure")
        },
        |(), _| {},
    )?;
    tokio::time::timeout(PATIENCE, opening.recv()).await??;
    let mut ready = Box::pin(worker.ready());
    assert!(
        ready.as_mut().now_or_never().is_none(),
        "Desktop readiness preceded insertion setup"
    );
    let permit = input.begin().context("Recording")?;
    let mut queued = Box::pin(inserter.insert(
        "fixture".into(),
        permit,
        Delivery::Paste,
        crate::Enter::Withheld,
    ));
    assert!(queued.as_mut().now_or_never().is_none());
    release.send(())?;
    assert!(
        tokio::time::timeout(PATIENCE, ready).await?.is_err(),
        "A failed setup reported readiness"
    );
    match received.try_recv() {
        Ok(Input::Unavailable(reason)) => assert!(
            reason.starts_with("X11 input is unavailable: Synthetic setup failure."),
            "{reason}"
        ),
        other => bail!("Readiness failed before setup reported why: {other:?}"),
    }
    let answer = tokio::time::timeout(PATIENCE, queued)
        .await
        .context("A queued insertion was left waiting")?;
    assert!(answer.is_err());
    Ok(())
}

#[tokio::test]
async fn a_panicking_insertion_thread_answers_the_request_in_hand_and_every_queued_one()
-> anyhow::Result<()> {
    let (events, received) = async_channel::bounded(4);
    let input = InputSender::new(events);
    let (inserter, requests) = crate::Inserter::lane(crate::FocusReport::default().subscribe());
    let (taken, holding) = async_channel::bounded(1);
    let (release, released) = mpsc::channel();
    let worker = InsertionWorker::spawn(
        input.clone(),
        requests,
        || Ok(()),
        move |(), requests| {
            let _in_hand = requests.recv_blocking();
            taken
                .try_send(())
                .expect("The test awaits the request in hand");
            released
                .recv_timeout(PATIENCE)
                .expect("The test queues a second request");

            panic::resume_unwind(Box::new("private payload"));
        },
    )?;
    worker.ready().await?;
    let permit = input.begin().context("Recording")?;
    let mut in_hand = Box::pin(inserter.insert(
        "fixture".into(),
        permit.clone(),
        Delivery::Paste,
        crate::Enter::Withheld,
    ));
    assert!(in_hand.as_mut().now_or_never().is_none());
    tokio::time::timeout(PATIENCE, holding.recv()).await??;
    let mut queued = Box::pin(inserter.insert(
        "fixture".into(),
        permit,
        Delivery::Paste,
        crate::Enter::Withheld,
    ));
    assert!(queued.as_mut().now_or_never().is_none());
    release.send(())?;
    for answer in [in_hand, queued] {
        let answer = tokio::time::timeout(PATIENCE, answer)
            .await
            .context("An insertion request was left waiting")?;
        assert!(answer.is_err());
    }
    match received.try_recv() {
        Ok(Input::Unavailable(reason)) => assert!(!reason.contains("private payload")),
        other => bail!("The stopped insertion thread did not report why: {other:?}"),
    }
    Ok(())
}
