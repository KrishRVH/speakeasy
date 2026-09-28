use async_channel::Sender;
use raw_window_handle::RawWindowHandle;
mod process;
pub use process::ProcessGroup;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

/// The hold-to-talk shortcut as people see it.
#[cfg(target_os = "macos")]
pub const SHORTCUT: &str = "Fn";
#[cfg(not(target_os = "macos"))]
pub const SHORTCUT: &str = "Ctrl + Win";

#[derive(Debug, Clone, Copy)]
pub enum Input {
    Press,
    Release,
    Lock,
    Cancel,
    Toggle,
    Quit,
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

    /// Space while held locks hands-free. Returns whether to swallow the event,
    /// so neither the press nor its release types a space or switches input.
    fn space(&mut self, down: bool) -> bool {
        if down && self.held {
            self.space = true;
        }
        down && self.held || !down && std::mem::take(&mut self.space)
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
    if matches!(event, Input::Cancel | Input::Quit) {
        tx.cancel();
    }
    if tx.sender.try_send(event).is_err() {
        tx.close();
    }
}

/// A single atomic commit gate bridges an OS callback and the session owner.
/// Clipboard preparation may block; Escape must still be able to invalidate
/// the pending paste. Successful compare_exchange is the commit point.
#[derive(Clone)]
pub struct InputSender {
    sender: Sender<Input>,
    gate: Arc<AtomicU8>,
}
impl InputSender {
    pub fn new(sender: Sender<Input>) -> Self {
        Self {
            sender,
            gate: Arc::new(AtomicU8::new(0)),
        }
    }
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    pub fn begin(&self) {
        self.gate.store(1, Ordering::Release);
    }
    pub fn active(&self) -> bool {
        self.gate.load(Ordering::Acquire) == 1
    }
    pub fn commit(&self) -> bool {
        self.gate
            .compare_exchange(1, 3, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn cancel(&self) {
        let _ = self
            .gate
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn close(&self) {
        self.cancel();
        self.sender.close();
    }
}

#[derive(Debug)]
pub enum Inserted {
    Sent,
    Copied(&'static str),
    Cancelled,
    Unavailable(&'static str),
}

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod preview {
    use super::*;
    pub struct InputMonitor;
    impl InputMonitor {
        pub fn start(_: InputSender) -> anyhow::Result<Self> {
            anyhow::bail!(
                "System dictation requires Windows or macOS. Use --demo for the pill preview."
            )
        }
    }
    pub fn configure_pill(_: RawWindowHandle) -> anyhow::Result<()> {
        Ok(())
    }
    pub fn set_pill_visible(_: RawWindowHandle, _: bool) {}
    pub fn modifiers_down() -> bool {
        false
    }
    pub fn insert(_: &str, _: &InputSender, _: bool) -> anyhow::Result<Inserted> {
        anyhow::bail!("Text insertion is available on Windows and macOS.")
    }
    pub fn show_error(message: &str) {
        eprintln!("Speakeasy: {message}");
    }
    pub fn reduced_motion() -> bool {
        false
    }
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use preview::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_during_clipboard_preparation_prevents_commit() {
        let (tx, _rx) = async_channel::bounded(4);
        let input = InputSender::new(tx);
        input.begin();
        assert!(input.active());
        // A native callback can invalidate preparation while the owner is in
        // an OS clipboard call. No queue-drain or UI tick is required.
        deliver(&input, Input::Cancel);
        assert!(!input.commit());
        input.begin();
        assert!(input.commit());
        deliver(&input, Input::Cancel);
        assert!(!input.commit()); // never submit the same result twice
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
    fn full_input_queue_cancels_instead_of_losing_escape() {
        let (tx, rx) = async_channel::bounded(1);
        let input = InputSender::new(tx);
        input.begin();
        deliver(&input, Input::Press);
        deliver(&input, Input::Cancel);
        assert!(rx.is_closed());
        assert!(!input.commit());
    }
}
