use async_channel::Sender;
use raw_window_handle::RawWindowHandle;
mod process;
pub use process::ProcessGroup;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// The hold-to-talk shortcut as people see it.
#[cfg(target_os = "macos")]
pub const SHORTCUT: &str = "Fn";
#[cfg(target_os = "windows")]
pub const SHORTCUT: &str = "Ctrl + Win";
#[cfg(target_os = "linux")]
pub const SHORTCUT: &str = "Ctrl + Super + Space";

#[derive(Debug, Clone)]
pub enum Input {
    Press,
    Release,
    Lock,
    Cancel,
    Toggle,
    Quit,
    Unavailable(String),
    DesktopReady { shortcut: String, cancel: String },
}

// Publishing the native wake handle and requesting stop share one lock. A stop
// before initialization stays effective, and a wake cannot use a retired handle.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
struct MonitorControl<T> {
    state: std::sync::Mutex<MonitorState<T>>,
}
#[cfg(any(target_os = "windows", target_os = "macos", test))]
struct MonitorState<T> {
    stopping: bool,
    native: Option<T>,
}
#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl<T> Default for MonitorControl<T> {
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
    fn start(&self, native: T, ready: impl FnOnce()) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.stopping {
            return false;
        }
        state.native = Some(native);
        ready();
        true
    }

    fn request_stop(&self, wake: impl FnOnce(&T)) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.stopping = true;
        if let Some(native) = &state.native {
            wake(native);
        }
    }

    fn stopping(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stopping
    }

    fn clear(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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
    if matches!(event, Input::Cancel | Input::Quit | Input::Unavailable(_)) {
        tx.cancel();
    }
    if tx.sender.try_send(event).is_err() {
        tx.close();
    }
}

/// A generation-aware commit gate bridges an OS callback and the session owner.
/// Clipboard preparation may block; Escape must still be able to invalidate
/// the pending paste. Successful compare_exchange is the commit point.
#[derive(Clone)]
pub struct InputSender {
    sender: Sender<Input>,
    gate: Arc<AtomicU64>,
}
/// Authorization for exactly one recording. Starting another recording never
/// authorizes an old clipboard operation that is still preparing its input.
#[derive(Clone)]
pub struct InsertPermit {
    gate: Arc<AtomicU64>,
    generation: u64,
}
impl InsertPermit {
    #[cfg(target_os = "linux")]
    fn same_recording(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.gate, &other.gate) && self.generation == other.generation
    }
    #[cfg(target_os = "linux")]
    fn revoke(&self) {
        let _ = self.gate.compare_exchange(
            self.generation,
            self.generation | 2,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
    pub fn active(&self) -> bool {
        self.gate.load(Ordering::Acquire) == self.generation
    }
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
    pub fn new(sender: Sender<Input>) -> Self {
        Self {
            sender,
            gate: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    pub fn begin(&self) -> Option<InsertPermit> {
        let previous = self
            .gate
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                (old & !3).checked_add(4).map(|next| next | 1)
            })
            .ok()?;
        Some(InsertPermit {
            gate: self.gate.clone(),
            generation: ((previous & !3) + 4) | 1,
        })
    }
    pub fn active(&self) -> bool {
        self.gate.load(Ordering::Acquire) & 3 == 1
    }
    fn cancel(&self) {
        let _ = self
            .gate
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                (old & 3 == 1).then_some((old & !3) | 2)
            });
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

#[derive(Clone)]
pub struct DesktopOptions {
    pub shortcut: String,
    pub cancel: String,
    pub terminal_paste: bool,
    pub manual_paste: bool,
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
pub struct Inserter {
    serial: Arc<tokio::sync::Mutex<()>>,
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
impl Inserter {
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
    use super::*;

    #[test]
    fn monitor_stop_before_delayed_start_never_publishes_ready() -> anyhow::Result<()> {
        let control = Arc::new(MonitorControl::default());
        let native = control.clone();
        let (initialized, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (ready, events) = async_channel::bounded(1);
        let thread = std::thread::spawn(move || {
            let _ = initialized.send(());
            let _ = released.recv();
            let started = native.start(42, || {
                let _ = ready.try_send(());
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
            .map_err(|_| anyhow::anyhow!("Fake monitor failed"))?;
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
