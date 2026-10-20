//! Native desktop input, presentation, and child-process ownership.
//!
//! Callbacks deliver events to the session owner. Insertion permits authorize one recording and can
//! be revoked while a native clipboard call is blocked.

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
compile_error!("Speakeasy supports Windows, macOS, and Linux.");

mod insertion;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod keyboard;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod monitor;
mod owned_thread;
mod process;
mod screen;
#[cfg(target_os = "windows")]
mod windows;

use std::{
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_channel::Sender;
use raw_window_handle::WindowHandle;

#[cfg(target_os = "linux")]
use self::linux as native;
#[cfg(target_os = "linux")]
pub use self::linux::{
    APPLICATION_ID, InputMonitor, NativeTray, TrayAction, TrayPresentation, copy_text, prepare,
    reduced_motion, show_startup_error,
};
#[cfg(target_os = "macos")]
use self::macos as native;
#[cfg(target_os = "macos")]
pub use self::macos::{copy_text, reduced_motion, show_startup_error};
#[cfg(target_os = "windows")]
use self::windows as native;
#[cfg(target_os = "windows")]
pub use self::windows::{attach_console, copy_text, reduced_motion, show_startup_error};
pub use self::{
    insertion::{Delivery, Inserter},
    owned_thread::OwnedThread,
    process::ProcessGroup,
    screen::{Display, Frame, PointerMonitor, PointerSample, ScreenCapture, ScreenEvent},
};

/// The pill window's width in device-independent pixels.
pub const PILL_WIDTH: u16 = 400;
/// The pill window's height in device-independent pixels.
pub const PILL_HEIGHT: u16 = 100;
/// The gap in device-independent pixels between the pill window and the bottom of the work area.
pub const PILL_MARGIN: u16 = 2;

/// The hold-to-talk shortcut as people see it.
#[cfg(target_os = "macos")]
pub const SHORTCUT: &str = "Fn";
/// The hold-to-talk shortcut as people see it.
#[cfg(target_os = "windows")]
pub const SHORTCUT: &str = "Ctrl + Win";
/// The hold-to-talk shortcut as people see it.
#[cfg(target_os = "linux")]
pub const SHORTCUT: &str = "Ctrl + Super + Space";

/// The shortcut that discards a recording, as people see it.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub const CANCEL_SHORTCUT: &str = "Escape";
/// The shortcut that discards a recording, as people see it.
#[cfg(target_os = "linux")]
pub const CANCEL_SHORTCUT: &str = "Ctrl + Super + Escape";

/// A live window of this process, for the native presentation GPUI does not offer.
///
/// Build it from the handle GPUI lends for an open window and use it before that window closes.
/// It is not `Send` on any platform, so it stays on the UI thread that owns the window: Windows
/// refuses a window another thread owns, and macOS requires the main thread. Showing, hiding, and
/// resizing can call back into GPUI synchronously, so call those methods with GPUI's borrows
/// released.
pub struct NativeWindow {
    native: native::Window,
    /// Not `Send` even where the native window would be, so code checked on one host keeps the
    /// thread rule every platform needs.
    _ui_thread: PhantomData<*const ()>,
}

impl NativeWindow {
    /// The window behind `handle`, a window of this process.
    ///
    /// # Errors
    /// Fails for a handle of another windowing system, or one this thread may not use: on Windows a
    /// window another thread owns, on macOS any thread but the main one.
    pub fn new(handle: WindowHandle<'_>) -> anyhow::Result<Self> {
        Ok(Self {
            native: native::Window::new(handle)?,
            _ui_thread: PhantomData,
        })
    }
}

/// Whether dictation runs, as the tray and Settings present it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    /// Dictation resources are live.
    Running,
    /// Dictation resources are being released.
    Pausing,
    /// Dictation is off.
    Paused,
}

/// What dictation is doing, as the tray presents it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// No recording is in progress.
    Idle,
    /// Capture is starting or recording.
    Dictating,
    /// Capture is ending or speech is being transcribed.
    Finishing,
}

/// Desktop observations and explicit dictation commands sent to the owner.
#[derive(Debug, Clone)]
pub enum Input {
    /// The physical shortcut became held.
    Press,
    /// The physical shortcut was released.
    Release,
    /// Space during a hold requests hands-free capture.
    Lock,
    /// S during a hold turns the recording into a Show, which also captures the screen.
    Show,
    /// Discards the active session and revokes pending insertion.
    Cancel,
    /// Starts or finishes hands-free capture.
    Toggle,
    /// Desktop access failed; the message explains how to resume.
    Unavailable(String),
    /// Shortcut and insertion preparation has completed.
    DesktopReady {
        /// The accepted dictation shortcut for display in Settings.
        shortcut: String,
        /// The accepted cancellation shortcut for display in Settings.
        cancel: String,
    },
}

/// The lane from OS callbacks to the session owner, carrying the recording's commit gate.
///
/// Clipboard preparation may block; Escape must still be able to invalidate the pending paste. A
/// permit's successful commit is the point of no return.
#[derive(Clone)]
pub struct InputSender {
    sender: Sender<Input>,
    gate: Gate,
}

impl InputSender {
    /// Creates an input lane with no recording authorized.
    #[must_use]
    pub fn new(sender: Sender<Input>) -> Self {
        Self {
            sender,
            gate: Gate::default(),
        }
    }

    /// Queues `event` for the owner without waiting.
    ///
    /// Cancellation and desktop failure revoke the current recording before the owner sees them. A
    /// full queue closes the lane, disabling dictation rather than losing an Escape or a release.
    pub fn deliver(&self, event: Input) {
        if matches!(event, Input::Cancel | Input::Unavailable(_)) {
            self.cancel();
        }
        if self.sender.try_send(event).is_err() {
            self.close();
        }
    }

    /// Reports that desktop access failed, then disables native input and insertion.
    pub fn fail(&self, message: String) {
        self.deliver(Input::Unavailable(message));
        self.close();
    }

    /// Whether native input and insertion are permanently disabled.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// Resolves once native input and insertion are permanently disabled.
    #[cfg(target_os = "linux")]
    pub(crate) fn closed(&self) -> impl Future<Output = ()> + '_ {
        self.sender.closed()
    }

    /// Authorizes a new recording, invalidating every older permit.
    ///
    /// Returns `None` once the lane is closed or generations run out.
    #[must_use]
    pub fn begin(&self) -> Option<InsertPermit> {
        if self.is_closed() {
            return None;
        }
        let generation = self.gate.advance()?;
        let permit = InsertPermit {
            gate: self.gate.clone(),
            generation,
        };
        // A close racing this call may have revoked the previous generation instead of this one.
        if self.is_closed() {
            permit.revoke();
            return None;
        }
        Some(permit)
    }

    /// Whether the current recording still has insertion authority.
    #[must_use]
    pub fn active(&self) -> bool {
        self.gate.is_authorized()
    }

    /// Revokes the current recording without waiting for the owner.
    pub fn cancel(&self) {
        self.gate.revoke_current();
    }

    /// Disables native input and revokes every outstanding insertion permit.
    pub fn close(&self) {
        self.sender.close();
        self.cancel();
    }
}

/// Authorization for exactly one recording. Starting another recording never authorizes an old
/// clipboard operation that is still preparing its input.
#[derive(Clone)]
pub struct InsertPermit {
    gate: Gate,
    generation: u64,
}

impl InsertPermit {
    #[cfg(target_os = "linux")]
    fn same_recording(&self, other: &Self) -> bool {
        self.gate.is_same(&other.gate) && self.generation == other.generation
    }

    /// Revokes only this recording, including while an OS operation is blocked. An obsolete permit
    /// cannot revoke a newer recording.
    pub fn revoke(&self) {
        self.gate.revoke(self.generation);
    }

    /// Whether this recording still has insertion authority.
    #[must_use]
    pub fn active(&self) -> bool {
        self.gate.is_current(self.generation)
    }

    /// Consumes this recording’s authority immediately before native input is submitted, returning
    /// whether it still held authority.
    #[must_use]
    pub fn commit(&self) -> bool {
        self.gate.revoke(self.generation)
    }
}

/// The current recording generation, shared with the callbacks that revoke it.
///
/// Generations count up above two state bits. A permit holds authority only while the gate equals
/// its authorized generation, so only one revocation of it can succeed.
#[derive(Clone, Default)]
#[expect(
    clippy::disallowed_types,
    reason = "Native Escape must revoke this recording while its owner is blocked in a clipboard OS call"
)]
struct Gate(Arc<AtomicU64>);

impl Gate {
    const AUTHORIZED: u64 = 1;
    const GENERATION_STEP: u64 = 4;
    const REVOKED: u64 = 2;
    const STATE: u64 = Self::AUTHORIZED | Self::REVOKED;

    /// Authorizes the next generation, superseding the current one; none once generations run out.
    fn advance(&self) -> Option<u64> {
        let next = |current: u64| {
            Some((current & !Self::STATE).checked_add(Self::GENERATION_STEP)? | Self::AUTHORIZED)
        };
        let previous = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, next)
            .ok()?;
        next(previous)
    }

    /// Revokes `generation` if it is still current, reporting whether it was.
    fn revoke(&self, generation: u64) -> bool {
        self.0
            .compare_exchange(
                generation,
                generation | Self::REVOKED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn revoke_current(&self) {
        self.0.fetch_or(Self::REVOKED, Ordering::AcqRel);
    }

    fn is_current(&self, generation: u64) -> bool {
        self.0.load(Ordering::Acquire) == generation
    }

    fn is_authorized(&self) -> bool {
        self.0.load(Ordering::Acquire) & Self::STATE == Self::AUTHORIZED
    }

    #[cfg(target_os = "linux")]
    fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Submission outcome; native input cannot prove that the target accepted text.
#[derive(Debug, PartialEq, Eq)]
pub enum Inserted {
    /// Native input was submitted once.
    Sent,
    /// Text remains on the clipboard with a manual-paste remedy.
    Copied(&'static str),
    /// Insertion authority was revoked before submission.
    Cancelled,
    /// Insertion is unavailable or incomplete; the message explains how to recover.
    Unavailable(&'static str),
}

/// Explicit desktop shortcut and insertion choices.
#[derive(Clone)]
pub struct DesktopOptions {
    /// Reserved Linux dictation chord in portal syntax.
    pub shortcut: String,
    /// Reserved Linux cancellation chord in portal syntax.
    pub cancel: String,
    /// Whether clipboard submission pastes with Ctrl+Shift+V.
    pub terminal_paste: bool,
    /// Whether text is only copied, for the user to paste without keyboard-control permission.
    pub manual_paste: bool,
    /// Whether desktop command bindings replace observing a native shortcut.
    pub external_shortcut: bool,
}

impl Default for DesktopOptions {
    fn default() -> Self {
        Self {
            shortcut: "CTRL+LOGO+space".into(),
            cancel: "CTRL+LOGO+Escape".into(),
            terminal_paste: false,
            manual_paste: false,
            external_shortcut: false,
        }
    }
}

/// Owns desktop observation and the insertion thread until explicit stop and acknowledged cleanup.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub struct InputMonitor {
    hook: native::HookMonitor,
    insertion: insertion::Worker,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl InputMonitor {
    /// Requests native observation and insertion shutdown without joining their threads.
    pub fn request_stop(&self) {
        self.hook.request_stop();
        self.insertion.request_stop();
    }

    /// Resolves once both threads have retired their native resources; await it before replacing
    /// or dropping the monitor.
    pub fn stopped(&self) -> impl Future<Output = ()> + use<> {
        let hook = self.hook.stopped();
        let insertion = self.insertion.stopped();
        async move {
            hook.await;
            insertion.await;
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Starts desktop observation and the insertion thread that serves its matching [`Inserter`].
///
/// # Errors
/// Returns thread startup or native desktop initialization failures.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn prepare(input: InputSender, _: DesktopOptions) -> anyhow::Result<(InputMonitor, Inserter)> {
    let (insertion, inserter) = insertion::Worker::start(input.clone())?;
    let hook = native::HookMonitor::start(input)?;
    Ok((InputMonitor { hook, insertion }, inserter))
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    #[test]
    fn closed_input_cannot_grant_new_insertion_authority() -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Missing initial permit")?;
        input.close();
        assert!(!permit.active());
        assert!(input.begin().is_none());
        assert!(!input.active());
        Ok(())
    }

    #[test]
    fn escape_during_clipboard_preparation_prevents_commit() -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(4);
        let input = InputSender::new(sender);
        let permit = input.begin().context("First insertion permit")?;
        assert!(input.active());
        input.deliver(Input::Cancel);
        assert!(
            !permit.commit(),
            "Escape must revoke without the owner draining its queue"
        );
        let next = input.begin().context("New insertion permit")?;
        assert!(!permit.commit(), "New recording authorized an old paste");
        assert!(next.commit());
        input.deliver(Input::Cancel);
        assert!(!next.commit(), "The same result was submitted twice");
        Ok(())
    }

    #[test]
    fn obsolete_permit_cannot_revoke_a_newer_recording() -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(4);
        let input = InputSender::new(sender);
        let obsolete = input.begin().context("Obsolete recording")?;
        let current = input.begin().context("Current recording")?;
        obsolete.revoke();
        assert!(current.active());
        assert!(current.commit());
        Ok(())
    }

    #[test]
    fn full_input_queue_cancels_instead_of_losing_escape() -> anyhow::Result<()> {
        let (sender, events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Insertion permit")?;
        assert!(permit.active());
        input.deliver(Input::Press);
        input.deliver(Input::Cancel);
        assert!(events.is_closed());
        assert!(!permit.commit());
        Ok(())
    }
}
