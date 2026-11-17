#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
compile_error!("Speakeasy supports Windows, macOS, and Linux.");

mod awake;
mod clipboard;
mod desktop;
mod image;
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
mod movie;
mod owned_thread;
mod permissions;
mod process;
mod screen;
#[cfg(target_os = "windows")]
mod windows;
mod wsl;

#[cfg(target_os = "linux")]
use self::linux as native;
#[cfg(target_os = "linux")]
pub use self::linux::{
    APPLICATION_ID, DisplayServer, NativeTray, TrayAction, TrayPresentation, disable_rtkit,
    display_server,
};
#[cfg(target_os = "macos")]
use self::macos as native;
#[cfg(target_os = "macos")]
pub use self::macos::StatusItem;
#[cfg(target_os = "windows")]
use self::windows as native;
#[cfg(target_os = "windows")]
pub use self::windows::installation::installed_directory;
#[cfg(target_os = "windows")]
pub use self::windows::login::{start_at_login, startup_approved};
pub use self::{
    awake::Awake,
    clipboard::{ClipboardContent, copy_content},
    desktop::{
        DesktopOptions, FileFilter, InputMonitor, LinuxPaste, MacShortcut, NativeWindow, Shortcut,
        WindowsShortcut, attach_stdio, copy_text, prepare, reduced_motion, secure_input,
        show_startup_error,
    },
    image::BgraImage,
    input::{IdleInput, Input, InputSender, InsertPermit, Taps},
    insertion::{Delivery, Enter, FocusReport, Inserted, Inserter, OwnFocus},
    movie::{MovieControl, MovieEvent, MovieFrame, MovieTarget, read_movie_frame},
    owned_thread::{OwnedThread, StoppableThread},
    permissions::{Permission, Permissions},
    process::ProcessGroup,
    screen::{
        Display, Frame, Part, PlacedWindow, PointerMonitor, PointerSample, Scope, ScreenCapture,
        ScreenEvent, Target, WindowGone, WindowLayer, WindowLayout, WindowWatch,
    },
    wsl::wsl,
};

pub const PILL_WIDTH: u16 = 400;

pub const PILL_HEIGHT: u16 = 100;

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) const PILL_MARGIN: u16 = 2;

#[cfg(test)]
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(target_os = "macos")]
pub const SHORTCUT: &str = "Fn";

#[cfg(target_os = "windows")]
pub const SHORTCUT: &str = "Ctrl + Win";

#[cfg(target_os = "linux")]
pub const SHORTCUT: &str = "Ctrl + Super + Space";

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub const CANCEL_SHORTCUT: &str = "Escape";

#[cfg(target_os = "linux")]
pub const CANCEL_SHORTCUT: &str = "Ctrl + Super + Escape";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayItem {
    pub label: &'static str,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayMenu {
    pub pause: TrayItem,
    pub toggle: TrayItem,
    pub cancel: TrayItem,
    pub paste_last: TrayItem,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub show_screen: TrayItem,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub show_area: TrayItem,
}
