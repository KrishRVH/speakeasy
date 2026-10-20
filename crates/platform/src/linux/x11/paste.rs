//! X11 insertion: waits for released shortcut keys, copies, confirms clipboard ownership and focus,
//! then sends the paste shortcut once through XTEST.

use std::{
    ops::ControlFlow,
    panic::{self, AssertUnwindSafe},
};

use anyhow::ensure;
use async_channel::Receiver;
use x11rb::{
    protocol::{
        xkb,
        xkb::ConnectionExt as _,
        xproto::{self, ConnectionExt as _, Window},
        xtest::ConnectionExt as _,
    },
    xcb_ffi::XCBConnection,
};
use xkbcommon::xkb::Keysym;

use super::{
    Keyboard,
    clipboard::{Clipboard, ClipboardAccess as _},
    has_external_target,
};
use crate::{
    Delivery, DesktopOptions, InputSender, Inserted, OwnedThread,
    insertion::{Insertion, PASTE_AFTER_RELEASE, abandon_queued, wait_for_released_modifiers},
    linux::{COPIED_FOR_MANUAL_PASTE, HostSession, NO_PASTE_BINDING},
};

/// Serializes X11 insertion on its own thread. Dropping it revokes the current permit and closes its
/// queue, then joins the thread; the desktop thread closes the input lane after reporting why it
/// stopped.
///
/// Every request shares one X connection, so a failed setup or a panic ends the thread instead of
/// serving on with a connection in an unknown state. It reports why, then abandons each queued
/// request, which answers its caller.
pub(super) struct InsertionWorker {
    _thread: OwnedThread,
    input: InputSender,
    requests: Receiver<Insertion>,
}

impl InsertionWorker {
    pub(super) fn spawn(
        input: InputSender,
        options: DesktopOptions,
        requests: Receiver<Insertion>,
    ) -> anyhow::Result<Self> {
        Self::spawn_with(input, requests, move |requests| {
            serve_insertions(requests, &options)
        })
    }

    fn spawn_with(
        input: InputSender,
        requests: Receiver<Insertion>,
        serve: impl FnOnce(&Receiver<Insertion>) -> anyhow::Result<()> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let thread = OwnedThread::spawn("x11-insertion", {
            let (input, requests) = (input.clone(), requests.clone());
            move || {
                // Unwinding drops the request in hand, which answers its caller.
                match panic::catch_unwind(AssertUnwindSafe(|| serve(&requests))) {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => input.fail(format!(
                        "X11 input is unavailable: {error}. Check your desktop connection, then resume."
                    )),
                    // Never the panic payload, which could quote the text.
                    Err(_) => input.fail(
                        "X11 input stopped unexpectedly. Resume dictation to reconnect.".into(),
                    ),
                }
                abandon_queued(&requests);
            }
        })?;
        Ok(Self {
            _thread: thread,
            input,
            requests,
        })
    }
}

impl Drop for InsertionWorker {
    fn drop(&mut self) {
        self.input.cancel();
        self.requests.close();
    }
}

fn serve_insertions(
    requests: &Receiver<Insertion>,
    options: &DesktopOptions,
) -> anyhow::Result<()> {
    let mut clipboard = Clipboard::open()?;
    let keyboard = Keyboard::load(&clipboard.connection)?;
    while let Ok(request) = requests.recv_blocking() {
        let outcome = paste(&keyboard, &mut clipboard, &request, options);
        request.reply.send(outcome);
    }
    Ok(())
}

fn paste(
    keyboard: &Keyboard,
    clipboard: &mut Clipboard,
    request: &Insertion,
    options: &DesktopOptions,
) -> anyhow::Result<Inserted> {
    if !request.permit.active() {
        return Ok(Inserted::Cancelled);
    }
    if request.delivery == Delivery::Direct {
        return Ok(Inserted::Unavailable(
            "Direct text input is unavailable on this X11 desktop. Choose Insert: Paste in Settings.",
        ));
    }
    match copy_after_release(keyboard, clipboard, request, options)? {
        ControlFlow::Continue(owner) => paste_copied(keyboard, clipboard, request, options, owner),
        ControlFlow::Break(outcome) => Ok(outcome),
    }
}

fn copy_after_release(
    keyboard: &Keyboard,
    clipboard: &mut Clipboard,
    request: &Insertion,
    options: &DesktopOptions,
) -> anyhow::Result<ControlFlow<Inserted, Window>> {
    let previous_owner = clipboard.owner()?;
    let shortcut_held = !options.manual_paste && {
        let locks = keyboard.lock_modifiers();
        wait_for_released_modifiers(&request.permit, || {
            let keys = clipboard.connection.query_keymap()?.reply()?.keys;
            Ok(keyboard.held_modifiers(&keys) & !locks != 0)
        })?
    };
    let owner_changed = clipboard.owner()? != previous_owner;
    // Checked after that round trip, so a cancel during it never reaches the clipboard.
    if !request.permit.active() {
        return Ok(ControlFlow::Break(Inserted::Cancelled));
    }
    if owner_changed {
        return Ok(ControlFlow::Break(Inserted::Unavailable(
            "Clipboard changed while waiting for shortcut release. Dictate again when your keys are released.",
        )));
    }
    let Some(owner) = clipboard.set_text(&request.text)? else {
        return Ok(ControlFlow::Break(if request.permit.active() {
            Inserted::Unavailable("Clipboard changed before paste. Dictate again.")
        } else {
            Inserted::Cancelled
        }));
    };
    if options.manual_paste {
        return Ok(ControlFlow::Break(COPIED_FOR_MANUAL_PASTE));
    }
    if shortcut_held {
        return Ok(ControlFlow::Break(PASTE_AFTER_RELEASE));
    }
    Ok(ControlFlow::Continue(owner))
}

fn paste_copied(
    keyboard: &Keyboard,
    clipboard: &Clipboard,
    request: &Insertion,
    options: &DesktopOptions,
    owner: Window,
) -> anyhow::Result<Inserted> {
    let first_layout = uses_first_layout(&clipboard.connection).unwrap_or(false);
    let control = keyboard.unshifted_keycode(Keysym::Control_L);
    let shift = if options.terminal_paste {
        keyboard.unshifted_keycode(Keysym::Shift_L)
    } else {
        None
    };
    let v = keyboard.unshifted_keycode(Keysym::v);
    let (Some(control), Some(v)) = (control, v) else {
        return Ok(NO_PASTE_BINDING);
    };
    if !first_layout || (options.terminal_paste && shift.is_none()) {
        return Ok(Inserted::Copied(
            "Text copied. Automatic paste needs your first keyboard layout; paste manually with the current layout.",
        ));
    }
    if !clipboard.owns(owner)? {
        return Ok(Inserted::Unavailable(
            "Clipboard ownership changed before paste. Dictate again.",
        ));
    }
    let external = has_external_target(&clipboard.connection, HostSession::X11).unwrap_or(false);
    if let Some(outcome) = Delivery::Paste.check_focus(external) {
        return Ok(outcome);
    }
    if !request.permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    // Once committed, the key sequence completes even if cancellation arrives.
    let connection = &clipboard.connection;
    if submit_paste(
        control,
        shift,
        v,
        |kind, code| Ok(connection.xtest_fake_input(kind, code, 0, 0, 0, 0, 0)?),
        || {
            connection.get_input_focus()?.reply()?;
            Ok(())
        },
        |cookie| Ok(cookie.check()?),
    )
    .is_err()
    {
        return Ok(Inserted::Copied(
            "Text copied, but input submission failed. Paste manually; automatic paste was not repeated.",
        ));
    }
    Ok(Inserted::Sent)
}

fn uses_first_layout(connection: &XCBConnection) -> anyhow::Result<bool> {
    ensure!(
        connection.xkb_use_extension(1, 0)?.reply()?.supported,
        "XKB is unavailable"
    );
    let state = connection
        .xkb_get_state(xkb::ID::USE_CORE_KBD.into())?
        .reply()?;
    Ok(state.group == xkb::Group::M1)
}

fn submit_paste<C>(
    control: u8,
    shift: Option<u8>,
    v: u8,
    mut enqueue: impl FnMut(u8, u8) -> anyhow::Result<C>,
    synchronize: impl FnOnce() -> anyhow::Result<()>,
    mut check: impl FnMut(C) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut cookies = Vec::with_capacity(6);
    let mut failed = false;
    for code in [Some(control), shift, Some(v)].into_iter().flatten() {
        let Ok(cookie) = enqueue(xproto::KEY_PRESS_EVENT, code) else {
            failed = true;
            break;
        };
        cookies.push(cookie);
    }
    // Releases follow even a failed press. Checks wait for the barrier: a round trip per event
    // would hold synthetic modifiers down.
    for code in [Some(v), shift, Some(control)].into_iter().flatten() {
        match enqueue(xproto::KEY_RELEASE_EVENT, code) {
            Ok(cookie) => cookies.push(cookie),
            Err(_) => failed = true,
        }
    }
    synchronize()?;
    for cookie in cookies {
        failed |= check(cookie).is_err();
    }
    ensure!(!failed, "Input submission failed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{mem, sync::mpsc, time::Duration};

    use anyhow::{Context, bail};
    use futures_util::FutureExt;

    use super::*;
    use crate::Input;

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

    /// Runs `submit_paste` against fakes that refuse the enqueue at `fail_at` and, when asked,
    /// the first check, then proves every enqueue precedes the barrier and every check follows it.
    fn trace_paste(
        shift: Option<u8>,
        fail_at: Option<usize>,
        reject_first_check: bool,
    ) -> PasteTrace {
        let (log, steps) = mpsc::channel();
        let (enqueue_log, synchronize_log) = (log.clone(), log.clone());
        let mut positions = 0..;
        let mut reject_check = reject_first_check;
        let result = submit_paste(
            1,
            shift,
            3,
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

    /// Runs an insertion thread that takes one request, waits until a second queues behind it,
    /// then fails or panics. Returns the reason it reported once both requests were answered.
    async fn stop_insertion_thread(panics: bool) -> anyhow::Result<String> {
        let (events, received) = async_channel::bounded(4);
        let input = InputSender::new(events);
        let (inserter, requests) = crate::Inserter::lane();
        let (taken, holding) = async_channel::bounded(1);
        let (release, released) = mpsc::channel();
        let _worker = InsertionWorker::spawn_with(input.clone(), requests, move |requests| {
            let _in_hand = requests.recv_blocking()?;
            taken.try_send(())?;
            released.recv_timeout(Duration::from_secs(2))?;
            if panics {
                // Unwinds without the panic hook, so the payload never reaches test output.
                panic::resume_unwind(Box::new("private payload"));
            }
            bail!("Synthetic setup failure")
        })?;
        let permit = input.begin().context("Recording")?;
        let mut in_hand =
            Box::pin(inserter.insert("fixture".into(), permit.clone(), Delivery::Paste));
        assert!(in_hand.as_mut().now_or_never().is_none());
        tokio::time::timeout(Duration::from_secs(1), holding.recv()).await??;
        let mut queued = Box::pin(inserter.insert("fixture".into(), permit, Delivery::Paste));
        assert!(queued.as_mut().now_or_never().is_none());
        release.send(())?;
        for answer in [in_hand, queued] {
            let answer = tokio::time::timeout(Duration::from_secs(2), answer)
                .await
                .context("An insertion request was left waiting")?;
            assert!(answer.is_err());
        }
        match received.try_recv() {
            Ok(Input::Unavailable(reason)) => Ok(reason),
            _ => bail!("The stopped insertion thread did not report why"),
        }
    }

    #[tokio::test]
    async fn a_stopped_insertion_thread_answers_the_request_in_hand_and_every_queued_one()
    -> anyhow::Result<()> {
        assert!(
            stop_insertion_thread(false)
                .await?
                .starts_with("X11 input is unavailable: Synthetic setup failure.")
        );
        assert!(
            !stop_insertion_thread(true)
                .await?
                .contains("private payload")
        );
        Ok(())
    }
}
