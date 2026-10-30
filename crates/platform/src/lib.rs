//! Native desktop input, screen capture, presentation, child-process ownership, and the Windows
//! sign-in launch.
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
pub use self::linux::{APPLICATION_ID, NativeTray, TrayAction, TrayPresentation};
#[cfg(target_os = "macos")]
use self::macos as native;
#[cfg(target_os = "windows")]
use self::windows as native;
#[cfg(target_os = "windows")]
pub use self::windows::login::{start_at_login, startup_approved};
pub use self::{
    desktop::{
        DesktopOptions, FileFilter, InputMonitor, LinuxPaste, NativeWindow, attach_stdio,
        copy_text, prepare, reduced_motion, show_startup_error,
    },
    input::{Input, InputSender, InsertPermit, Taps},
    insertion::{Delivery, Inserted, Inserter},
    owned_thread::{OwnedThread, StoppableThread},
    process::ProcessGroup,
    screen::{
        Display, Frame, Part, PointerMonitor, PointerSample, Scope, ScreenCapture, ScreenEvent,
        Target, WindowGone,
    },
};

/// The pill window's width in device-independent pixels.
pub const PILL_WIDTH: u16 = 400;
/// The pill window's height in device-independent pixels.
pub const PILL_HEIGHT: u16 = 100;
/// The gap in device-independent pixels between the pill window and the bottom of the work area.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) const PILL_MARGIN: u16 = 2;

/// How long a test waits on a thread, lane, or answer before calling it stuck.
#[cfg(test)]
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

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
    /// Starts a hands-free Show of the display under the pointer.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub show_screen: TrayItem,
    /// Starts a hands-free Show of an area dragged out on the display under the pointer.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub show_area: TrayItem,
}
