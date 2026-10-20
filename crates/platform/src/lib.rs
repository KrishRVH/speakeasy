//! Native desktop input, presentation, and child-process ownership.
//!
//! Callbacks deliver events to the session owner. Insertion permits authorize one recording and can
//! be revoked while a native clipboard call is blocked.

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
compile_error!("Speakeasy supports Windows, macOS, and Linux.");

mod desktop;
mod input;
mod insertion;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod keyboard;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod monitor;
mod owned_thread;
mod process;
mod screen;
#[cfg(target_os = "windows")]
mod windows;

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
    desktop::NativeWindow,
    input::{Input, InputSender, InsertPermit},
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
