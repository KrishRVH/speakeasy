//! Native desktop input, screen capture, presentation, and child-process ownership.
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
// Linux only: the launcher and portal identity, and the StatusNotifierItem tray; Windows and
// macOS use `tray-icon` in the app.
#[cfg(target_os = "linux")]
pub use self::linux::{APPLICATION_ID, NativeTray, TrayAction, TrayPresentation};
#[cfg(target_os = "macos")]
use self::macos as native;
#[cfg(target_os = "windows")]
use self::windows as native;
// Windows only: the console a windowed executable lacks.
#[cfg(target_os = "windows")]
pub use self::windows::attach_console;
pub use self::{
    desktop::{InputMonitor, NativeWindow, copy_text, prepare, reduced_motion, show_startup_error},
    input::{Input, InputSender, InsertPermit},
    insertion::{Delivery, Inserter},
    owned_thread::{OwnedThread, StoppableThread},
    process::ProcessGroup,
    screen::{Display, Frame, PointerMonitor, PointerSample, ScreenCapture, ScreenEvent},
};

/// The pill window's width in device-independent pixels.
pub const PILL_WIDTH: u16 = 400;
/// The pill window's height in device-independent pixels.
pub const PILL_HEIGHT: u16 = 100;
/// The gap in device-independent pixels between the pill window and the bottom of the work area.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) const PILL_MARGIN: u16 = 2;

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

/// One dictation entry of the tray menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayItem {
    /// The text the entry shows.
    pub label: &'static str,
    /// Whether choosing the entry sends its command.
    pub enabled: bool,
}

/// The tray menu's dictation entries, already resolved by the app; native trays show them as given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayMenu {
    /// Pauses or resumes dictation.
    pub pause: TrayItem,
    /// Starts or finishes hands-free capture.
    pub toggle: TrayItem,
    /// Discards the active recording or recognition.
    pub cancel: TrayItem,
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

/// Shortcut and insertion choices for the Linux desktop; Windows and macOS read none of them.
#[derive(Clone, Debug)]
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
