use std::{mem, sync::mpsc};

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
    result: anyhow::Result<()>,
    enqueued: Vec<(u8, u8, bool)>,
    checks: usize,
}

fn trace_paste(shift: Option<u8>, fail_at: Option<usize>, reject_first_check: bool) -> PasteTrace {
    let (log, steps) = mpsc::channel();
    let (enqueue_log, synchronize_log) = (log.clone(), log.clone());
    let mut positions = 0..;
    let mut reject_check = reject_first_check;
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
        move || Ok(synchronize_log.send(Step::Synchronize)?),
        move |()| {
            log.send(Step::Check)?;
            ensure!(!mem::take(&mut reject_check), "Synthetic server rejection");
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
        let trace = trace_paste(shift, None, false);
        assert!(trace.result.is_ok());
        assert_eq!(trace.checks, if shift.is_some() { 6 } else { 4 });
        assert_eq!(
            trace.enqueued.last().map(|&(kind, code, _)| (kind, code)),
            Some(release(1))
        );
    }
    for fail_at in [None, Some(0), Some(2), Some(3)] {
        let trace = trace_paste(Some(2), fail_at, true);
        assert!(trace.result.is_err());
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
    let mut queued = Box::pin(inserter.insert("fixture".into(), permit, Delivery::Paste));
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
    let mut in_hand = Box::pin(inserter.insert("fixture".into(), permit.clone(), Delivery::Paste));
    assert!(in_hand.as_mut().now_or_never().is_none());
    tokio::time::timeout(PATIENCE, holding.recv()).await??;
    let mut queued = Box::pin(inserter.insert("fixture".into(), permit, Delivery::Paste));
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
