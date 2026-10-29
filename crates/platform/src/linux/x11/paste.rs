//! X11 insertion: waits for released shortcut keys, copies, confirms clipboard ownership, focus,
//! and released keys again, then sends the paste shortcut once through XTEST.

use std::{
    ops::ControlFlow,
    panic::{self, AssertUnwindSafe},
};

use anyhow::{Context as _, ensure};
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

use super::{
    Keyboard,
    clipboard::{Clipboard, ClipboardAccess as _},
};
use crate::{
    Delivery, DesktopOptions, InputSender, Inserted, LinuxPaste, OwnedThread,
    insertion::{
        Insertion, PASTE_AFTER_RELEASE, Reply, abandon_queued, wait_for_released_modifiers,
    },
    linux::{
        CLIPBOARD_CHANGED, COPIED_FOR_MANUAL_PASTE, HostSession, NO_PASTE_BINDING, PasteChord,
    },
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
    ready: Receiver<()>,
}

impl InsertionWorker {
    pub(super) fn start(
        input: InputSender,
        options: DesktopOptions,
        requests: Receiver<Insertion>,
    ) -> anyhow::Result<Self> {
        Self::spawn(
            input,
            requests,
            move || Paster::open(options),
            Paster::serve,
        )
    }

    /// Runs `open` on the worker thread, then `serve` with what it opened.
    fn spawn<S>(
        input: InputSender,
        requests: Receiver<Insertion>,
        open: impl FnOnce() -> anyhow::Result<S> + Send + 'static,
        serve: impl FnOnce(S, &Receiver<Insertion>) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (opened, ready) = Reply::channel();
        let thread = OwnedThread::spawn("x11-insertion", {
            let (input, requests) = (input.clone(), requests.clone());
            move || {
                // Unwinding drops the request in hand, which answers its caller. `opened` drops
                // only after a failed setup is reported, so that report precedes the desktop loop's
                // generic setup failure.
                let served = match panic::catch_unwind(AssertUnwindSafe(open)) {
                    Ok(Ok(state)) => {
                        opened.send(());
                        panic::catch_unwind(AssertUnwindSafe(|| serve(state, &requests))).map(Ok)
                    },
                    Ok(Err(error)) => Ok(Err(error)),
                    Err(panic) => Err(panic),
                };
                match served {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => input.fail(format!(
                        "X11 input is unavailable: {error}. Check your desktop connection, then resume dictation."
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
            ready,
        })
    }

    /// Resolves once the worker has opened its connection and read the keyboard mapping.
    ///
    /// # Errors
    /// Fails when setup failed, after the worker has reported why.
    pub(super) async fn ready(&self) -> anyhow::Result<()> {
        self.ready.recv().await.context("X11 input setup stopped")
    }
}

impl Drop for InsertionWorker {
    fn drop(&mut self) {
        self.input.cancel();
        self.requests.close();
    }
}

/// What the insertion thread owns: one X connection, the keyboard mapping it read, and the
/// options it pastes with.
struct Paster {
    clipboard: Clipboard,
    keyboard: Keyboard,
    options: DesktopOptions,
}

impl Paster {
    fn open(options: DesktopOptions) -> anyhow::Result<Self> {
        let clipboard = Clipboard::open()?;
        let keyboard = Keyboard::load(&clipboard.connection)?;
        Ok(Self {
            clipboard,
            keyboard,
            options,
        })
    }

    fn serve(mut self, requests: &Receiver<Insertion>) {
        while let Ok(request) = requests.recv_blocking() {
            let outcome = self.paste(&request);
            request.reply.send(outcome);
        }
    }

    fn paste(&mut self, request: &Insertion) -> anyhow::Result<Inserted> {
        if !request.permit.active() {
            return Ok(Inserted::Cancelled);
        }
        if request.delivery == Delivery::Direct {
            return Ok(Inserted::Unavailable(
                "Direct text input is unavailable on this X11 desktop. Choose Insert: Paste in Settings.",
            ));
        }
        match self.copy_after_release(request)? {
            ControlFlow::Continue(owner) => self.paste_copied(request, owner),
            ControlFlow::Break(outcome) => Ok(outcome),
        }
    }

    fn copy_after_release(
        &mut self,
        request: &Insertion,
    ) -> anyhow::Result<ControlFlow<Inserted, Window>> {
        let previous_owner = self.clipboard.owner()?;
        let shortcut_held = self.options.paste != LinuxPaste::Manual
            && wait_for_released_modifiers(&request.permit, || self.modifiers_held())?;
        let owner_changed = self.clipboard.owner()? != previous_owner;
        // Checked after that round trip, so a cancel during it never reaches the clipboard.
        if !request.permit.active() {
            return Ok(ControlFlow::Break(Inserted::Cancelled));
        }
        if owner_changed {
            return Ok(ControlFlow::Break(Inserted::Unavailable(
                "The clipboard changed while your shortcut keys were held. Release them, then dictate again.",
            )));
        }
        let Some(owner) = self.clipboard.set_text(&request.text)? else {
            return Ok(ControlFlow::Break(if request.permit.active() {
                CLIPBOARD_CHANGED
            } else {
                Inserted::Cancelled
            }));
        };
        if self.options.paste == LinuxPaste::Manual {
            return Ok(ControlFlow::Break(COPIED_FOR_MANUAL_PASTE));
        }
        if shortcut_held {
            return Ok(ControlFlow::Break(PASTE_AFTER_RELEASE));
        }
        Ok(ControlFlow::Continue(owner))
    }

    /// Whether a modifier other than a lock is down.
    fn modifiers_held(&self) -> anyhow::Result<bool> {
        let keys = self.clipboard.connection.query_keymap()?.reply()?.keys;
        Ok(self.keyboard.held_modifiers(&keys) & !self.keyboard.lock_modifiers() != 0)
    }

    fn paste_copied(&self, request: &Insertion, owner: Window) -> anyhow::Result<Inserted> {
        let terminal = self.options.paste == LinuxPaste::Terminal;
        // The first layout is the only one XTEST types through.
        let Some(chord) =
            PasteChord::resolve(terminal, |symbol| self.keyboard.unshifted_keycode(symbol))
        else {
            return Ok(NO_PASTE_BINDING);
        };
        let connection = &self.clipboard.connection;
        if !uses_first_layout(connection).unwrap_or(false) {
            return Ok(Inserted::Copied(
                "Text copied. Automatic paste needs your first keyboard layout; paste manually with the current layout.",
            ));
        }
        if !self.clipboard.owns(owner)? {
            return Ok(Inserted::Unavailable(
                "The clipboard changed before pasting. Dictate again.",
            ));
        }
        let external = self
            .clipboard
            .external_target(HostSession::X11)
            .unwrap_or(false);
        // Modifiers pressed during those round trips would join the synthetic paste chord.
        let released = !self.modifiers_held()?;
        if let Some(outcome) = Delivery::Paste.preflight(external, released) {
            return Ok(outcome);
        }
        if !request.permit.commit() {
            return Ok(Inserted::Cancelled);
        }
        // Once committed, the key sequence completes even if cancellation arrives.
        let submitted = submit_paste(
            chord,
            |kind, code| Ok(connection.xtest_fake_input(kind, code, 0, 0, 0, 0, 0)?),
            || {
                connection.get_input_focus()?.reply()?;
                Ok(())
            },
            |cookie| Ok(cookie.check()?),
        );
        Ok(match submitted {
            Ok(()) => Inserted::Sent,
            Err(_) => Inserted::Copied(
                "Text copied, but automatic paste failed and was not retried. Paste it manually.",
            ),
        })
    }
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
    chord: PasteChord<u8>,
    mut enqueue: impl FnMut(u8, u8) -> anyhow::Result<C>,
    synchronize: impl FnOnce() -> anyhow::Result<()>,
    mut check: impl FnMut(C) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut cookies = Vec::with_capacity(6);
    let mut failed = false;
    for code in chord.presses() {
        let Ok(cookie) = enqueue(xproto::KEY_PRESS_EVENT, code) else {
            failed = true;
            break;
        };
        cookies.push(cookie);
    }
    // Releases follow even a failed press. Checks wait for the barrier: a round trip per event
    // would hold synthetic modifiers down.
    for code in chord.presses().rev() {
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
    async fn readiness_waits_for_insertion_setup_and_follows_its_failure_report()
    -> anyhow::Result<()> {
        let (events, received) = async_channel::bounded(4);
        let input = InputSender::new(events);
        let (inserter, requests) = crate::Inserter::lane();
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
        let (inserter, requests) = crate::Inserter::lane();
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
                // Unwinds without the panic hook, so the payload never reaches test output.
                panic::resume_unwind(Box::new("private payload"));
            },
        )?;
        worker.ready().await?;
        let permit = input.begin().context("Recording")?;
        let mut in_hand =
            Box::pin(inserter.insert("fixture".into(), permit.clone(), Delivery::Paste));
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
}
