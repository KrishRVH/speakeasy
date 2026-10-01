//! Native desktop input, presentation, and child-process ownership.
//!
//! Callbacks deliver events to the session owner. Insertion permits authorize
//! one recording and can be revoked while a native clipboard call is blocked.

use async_channel::Sender;
use raw_window_handle::RawWindowHandle;
mod insertion;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod keyboard;
mod process;
pub use process::ProcessGroup;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// The hold-to-talk shortcut as people see it.
#[cfg(target_os = "macos")]
pub const SHORTCUT: &str = "Fn";
/// The hold-to-talk shortcut as people see it.
#[cfg(target_os = "windows")]
pub const SHORTCUT: &str = "Ctrl + Win";
/// The hold-to-talk shortcut as people see it.
#[cfg(target_os = "linux")]
pub const SHORTCUT: &str = "Ctrl + Super + Space";

#[derive(Debug, Clone)]
/// Desktop observations and explicit dictation commands sent to the owner.
pub enum Input {
    /// The physical shortcut became held.
    Press,
    /// The physical shortcut was released.
    Release,
    /// Space during a hold requests hands-free capture.
    Lock,
    /// Discard the active session and revoke pending insertion.
    Cancel,
    /// Start or finish hands-free capture.
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

// Publishing the native wake handle and requesting stop share one lock. A stop
// before initialization stays effective, and a wake cannot use a retired handle.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
struct MonitorControl<T> {
    #[expect(
        clippy::disallowed_types,
        reason = "Stop and native handle publication must be atomic while the native loop cannot receive owner messages"
    )]
    state: std::sync::Mutex<MonitorState<T>>,
}
#[cfg(any(target_os = "windows", target_os = "macos", test))]
struct MonitorState<T> {
    stopping: bool,
    native: Option<T>,
}
#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl<T> Default for MonitorControl<T> {
    #[expect(
        clippy::disallowed_types,
        reason = "The same lock serializes native handle publication, waking, and retirement to prevent waking a freed handle"
    )]
    fn default() -> Self {
        Self {
            state: std::sync::Mutex::new(MonitorState {
                stopping: false,
                native: None,
            }),
        }
    }
}
#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl<T> MonitorControl<T> {
    #[expect(
        clippy::significant_drop_tightening,
        reason = "Readiness must publish under the same lock as native initialization so stop cannot interleave"
    )]
    fn start(&self, native: T, ready: impl FnOnce()) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stopping {
            return false;
        }
        state.native = Some(native);
        ready();
        true
    }

    fn request_stop(&self, wake: impl FnOnce(&T)) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stopping = true;
        if let Some(native) = &state.native {
            wake(native);
        }
    }

    fn stopping(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopping
    }

    fn clear(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .native = None;
    }
}

/// Hold state for the modifier-only shortcut: Ctrl+Win on Windows, Fn on macOS.
/// Modifiers are observed, never swallowed. Space during a hold locks
/// hands-free; any other key belongs to a different shortcut, so it cancels
/// dictation and waits for the chord's release.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
#[derive(Default)]
struct Chord {
    held: bool,
    blocked: bool,
    space: bool,
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl Chord {
    /// `down` is the chord after this event; `fresh` marks a new press rather
    /// than autorepeat or a second key on the same side of the chord.
    fn modifiers(&mut self, down: bool, fresh: bool) -> Option<Input> {
        if !down {
            self.blocked = false;
            return std::mem::take(&mut self.held).then_some(Input::Release);
        }
        if fresh && !self.held && !self.blocked {
            self.held = true;
            return Some(Input::Press);
        }
        None
    }

    /// Space while held locks hands-free. Swallow it through its release,
    /// including repeats after the shortcut modifiers are released.
    fn space(&mut self, down: bool) -> bool {
        if down && self.held {
            self.space = true;
        }
        down && self.space || !down && std::mem::take(&mut self.space)
    }

    /// Another key ends an active hold. Returns whether one was interrupted.
    fn interrupt(&mut self) -> bool {
        let held = std::mem::take(&mut self.held);
        self.blocked |= held;
        held
    }
}

/// The hook never waits. Closing on overflow wakes the owner and disables
/// dictation rather than losing an Escape or a release and continuing unsafely.
pub fn deliver(tx: &InputSender, event: Input) {
    if matches!(event, Input::Cancel | Input::Unavailable(_)) {
        tx.cancel();
    }
    if tx.sender.try_send(event).is_err() {
        tx.close();
    }
}

/// A generation-aware commit gate bridges an OS callback and the session owner.
///
/// Clipboard preparation may block; Escape must still be able to invalidate
/// the pending paste. Successful `compare_exchange` is the commit point.
#[derive(Clone)]
pub struct InputSender {
    sender: Sender<Input>,
    #[expect(
        clippy::disallowed_types,
        reason = "Native Escape must revoke this recording while its owner is blocked in a clipboard OS call"
    )]
    gate: Arc<AtomicU64>,
}
/// Authorization for exactly one recording. Starting another recording never
/// authorizes an old clipboard operation that is still preparing its input.
#[derive(Clone)]
pub struct InsertPermit {
    #[expect(
        clippy::disallowed_types,
        reason = "Native Escape must revoke this recording while its owner is blocked in a clipboard OS call"
    )]
    gate: Arc<AtomicU64>,
    generation: u64,
}
impl InsertPermit {
    #[cfg(target_os = "linux")]
    fn same_recording(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.gate, &other.gate) && self.generation == other.generation
    }
    /// Revoke only this recording, including while an OS operation is blocked.
    /// An obsolete permit cannot revoke a newer recording.
    pub fn revoke(&self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A failed exchange means this permit was already revoked or superseded; it must never revoke a newer recording"
        )]
        let _ = self.gate.compare_exchange(
            self.generation,
            self.generation | 2,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
    /// Whether this recording still has insertion authority.
    #[must_use]
    pub fn active(&self) -> bool {
        self.gate.load(Ordering::Acquire) == self.generation
    }
    /// Consume this recording’s authority immediately before submitting native input.
    #[must_use]
    pub fn commit(&self) -> bool {
        self.gate
            .compare_exchange(
                self.generation,
                self.generation | 2,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}
impl InputSender {
    /// Create an input lane with no recording authorized.
    #[must_use]
    #[expect(
        clippy::disallowed_types,
        reason = "Construct the callback-visible generation gate; the owner cannot receive cancellation while blocked in native insertion"
    )]
    pub fn new(sender: Sender<Input>) -> Self {
        Self {
            sender,
            gate: Arc::new(AtomicU64::new(0)),
        }
    }
    /// Whether native input and insertion are permanently disabled.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    /// Authorize a new recording, invalidating every older permit; returns none after closure or generation exhaustion.
    #[must_use]
    pub fn begin(&self) -> Option<InsertPermit> {
        if self.is_closed() {
            return None;
        }
        let mut previous = self.gate.load(Ordering::Acquire);
        loop {
            let generation = (previous & !3).checked_add(4)? | 1;
            match self.gate.compare_exchange_weak(
                previous,
                generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let permit = InsertPermit {
                        gate: self.gate.clone(),
                        generation,
                    };
                    if self.is_closed() {
                        permit.revoke();
                        return None;
                    }
                    return Some(permit);
                },
                Err(current) => previous = current,
            }
        }
    }
    /// Whether this recording still has insertion authority.
    #[must_use]
    pub fn active(&self) -> bool {
        self.gate.load(Ordering::Acquire) & 3 == 1
    }
    /// Revoke the current recording without waiting for the owner.
    pub fn cancel(&self) {
        self.gate.fetch_or(2, Ordering::AcqRel);
    }
    /// Disable native input and revoke every outstanding insertion permit.
    pub fn close(&self) {
        self.sender.close();
        self.cancel();
    }
}

#[derive(Debug)]
/// Submission outcome; native input cannot prove that the target accepted text.
pub enum Inserted {
    /// Native input was submitted once.
    Sent,
    /// Text remains on the clipboard with a manual-paste remedy.
    Copied(&'static str),
    /// Insertion authority was revoked before submission.
    Cancelled,
    /// No text was submitted; the message explains how to retry.
    Unavailable(&'static str),
}

#[derive(Clone)]
/// Explicit desktop shortcut and insertion choices.
pub struct DesktopOptions {
    /// Reserved Linux dictation chord in portal syntax.
    pub shortcut: String,
    /// Reserved Linux cancellation chord in portal syntax.
    pub cancel: String,
    /// Use Ctrl+Shift+V when submitting through the clipboard.
    pub terminal_paste: bool,
    /// Copy text and let the user paste it without keyboard-control permission.
    pub manual_paste: bool,
    /// Use desktop command bindings instead of observing a native shortcut.
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

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(Clone)]
/// Serializes native clipboard/input operations without blocking the session owner.
pub struct Inserter {
    #[expect(
        clippy::disallowed_types,
        reason = "Serialize irreversible clipboard preparation on blocking native workers; cancellation remains on the independent generation gate"
    )]
    serial: Arc<tokio::sync::Mutex<()>>,
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
impl Inserter {
    /// Submit authorized text; `preserve` requests direct input instead of changing the clipboard.
    ///
    /// # Errors
    /// Returns native input failures or a failed blocking worker.
    pub async fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        preserve: bool,
    ) -> anyhow::Result<Inserted> {
        let serial = self.serial.clone().lock_owned().await;
        if !permit.active() {
            return Ok(Inserted::Cancelled);
        }
        tokio::task::spawn_blocking(move || {
            // One owned clipboard/input operation at a time. Waiting cancelled
            // requests never consume blocking threads or race a newer copy.
            let _serial = serial;
            let started = std::time::Instant::now();
            while permit.active()
                && modifiers_down()
                && started.elapsed() < std::time::Duration::from_millis(800)
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            insert(&text, &permit, preserve)
        })
        .await?
    }
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
/// Start desktop observation and create its matching insertion adapter.
///
/// # Errors
/// Returns thread startup or native desktop initialization failures.
#[expect(
    clippy::disallowed_types,
    reason = "The native insertion adapter serializes blocked clipboard calls while the generation gate handles cancellation"
)]
pub fn prepare(input: InputSender, _: DesktopOptions) -> anyhow::Result<(InputMonitor, Inserter)> {
    Ok((
        InputMonitor::start(input)?,
        Inserter {
            serial: Arc::new(tokio::sync::Mutex::new(())),
        },
    ))
}

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
compile_error!("Speakeasy supports Windows, macOS, and Linux.");

#[cfg(test)]
mod tests {
    #[test]
    fn closed_input_cannot_grant_new_insertion_authority() -> anyhow::Result<()> {
        let (sender, _receiver) = async_channel::bounded(1);
        let input = super::InputSender::new(sender);
        let permit = input
            .begin()
            .ok_or_else(|| anyhow::anyhow!("Missing initial permit"))?;
        input.close();
        assert!(!permit.active());
        assert!(input.begin().is_none());
        assert!(!input.active());
        Ok(())
    }

    use super::*;

    #[test]
    fn monitor_stop_before_delayed_start_never_publishes_ready() -> anyhow::Result<()> {
        let control = Arc::new(MonitorControl::default());
        let native = control.clone();
        let (initialized, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (ready, events) = async_channel::bounded(1);
        let thread = std::thread::spawn(move || {
            initialized
                .send(())
                .expect("Fake monitor startup lane is open");
            released.recv().expect("Test releases the fake monitor");
            let started = native.start(42, || {
                ready.try_send(()).expect("Fake readiness lane is open");
            });
            native.clear();
            started
        });
        entered.recv_timeout(std::time::Duration::from_secs(2))?;
        let mut woke = false;
        control.request_stop(|_| woke = true);
        release.send(())?;
        let started = thread
            .join()
            .map_err(|_panic| anyhow::anyhow!("Fake monitor failed"))?;
        assert!(!started);
        assert!(!woke);
        assert!(control.stopping());
        assert!(events.recv_blocking().is_err());
        // Retirement cannot make a stop-before-start monitor ready later.
        assert!(!control.start(43, || {}));
        Ok(())
    }

    #[test]
    fn monitor_stop_wakes_only_its_published_native_resource() {
        let control = MonitorControl::default();
        let mut ready = false;
        assert!(control.start(42, || ready = true));
        assert!(ready);
        let mut woken = None;
        control.request_stop(|native| woken = Some(*native));
        assert_eq!(woken, Some(42));
        control.clear();
        woken = None;
        control.request_stop(|native| woken = Some(*native));
        assert_eq!(woken, None);
        assert!(!control.start(43, || {}));
    }

    #[test]
    fn escape_during_clipboard_preparation_prevents_commit() -> anyhow::Result<()> {
        use anyhow::Context;
        let (tx, _rx) = async_channel::bounded(4);
        let input = InputSender::new(tx);
        let permit = input.begin().context("First insertion permit")?;
        assert!(input.active());
        // A native callback can invalidate preparation while the owner is in
        // an OS clipboard call. No queue-drain or UI tick is required.
        deliver(&input, Input::Cancel);
        assert!(!permit.commit());
        let next = input.begin().context("New insertion permit")?;
        assert!(!permit.commit(), "New recording authorized an old paste");
        #[cfg(target_os = "linux")]
        permit.revoke();
        assert!(next.commit());
        deliver(&input, Input::Cancel);
        assert!(!next.commit()); // never submit the same result twice
        Ok(())
    }

    #[test]
    fn chord_ignores_repeats_and_yields_to_other_shortcuts() {
        let mut chord = Chord::default();
        assert!(matches!(chord.modifiers(true, true), Some(Input::Press)));
        assert!(
            chord.modifiers(true, false).is_none(),
            "autorepeat restarted"
        );
        assert!(matches!(
            chord.modifiers(false, false),
            Some(Input::Release)
        ));
        // Ctrl+Win+Left belongs to Windows: cancel, then wait for release.
        assert!(matches!(chord.modifiers(true, true), Some(Input::Press)));
        assert!(chord.interrupt());
        assert!(
            chord.modifiers(true, true).is_none(),
            "interrupted chord restarted"
        );
        assert!(chord.modifiers(false, false).is_none());
        assert!(matches!(chord.modifiers(true, true), Some(Input::Press)));
        // Space locks hands-free; its press and release never reach the app.
        assert!(chord.space(true));
        assert!(chord.space(false));
        assert!(!chord.space(false));
        assert!(matches!(
            chord.modifiers(false, false),
            Some(Input::Release)
        ));
        assert!(!chord.space(true), "Space was swallowed without a hold");
    }

    #[test]
    fn full_input_queue_cancels_instead_of_losing_escape() -> anyhow::Result<()> {
        use anyhow::Context;
        let (tx, rx) = async_channel::bounded(1);
        let input = InputSender::new(tx);
        let permit = input.begin().context("Insertion permit")?;
        assert!(permit.active());
        deliver(&input, Input::Press);
        deliver(&input, Input::Cancel);
        assert!(rx.is_closed());
        assert!(!permit.commit());
        Ok(())
    }

    #[test]
    fn hands_free_space_stays_swallowed_until_its_release() {
        let mut chord = Chord::default();
        assert!(matches!(chord.modifiers(true, true), Some(Input::Press)));
        assert!(chord.space(true));
        assert!(
            chord.space(true),
            "A held shortcut must swallow Space repeats"
        );
        assert!(matches!(
            chord.modifiers(false, false),
            Some(Input::Release)
        ));
        assert!(
            chord.space(true),
            "Releasing the shortcut must not leak a held Space into the editor"
        );
        assert!(chord.space(false));
        assert!(!chord.space(true), "A fresh Space belongs to the editor");
        assert!(!chord.space(false));
    }
}
