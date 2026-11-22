use std::{fmt, marker::PhantomData};

use raw_window_handle::WindowHandle;

use crate::{InputSender, Inserter, OwnFocus, OwnedThread, native};

/// Owns desktop observation and, on Windows and macOS, the insertion thread.
///
/// Create, stop, and drop it on the UI thread. Drop joins its threads; await `stopped` first so
/// the join does not block.
pub struct InputMonitor {
    /// Declared first: drop joins it before `native` removes the macOS lifecycle observers.
    thread: OwnedThread,
    native: native::Monitor,
    /// Keeps the monitor `!Send` on every host, as the macOS lifecycle observers require.
    _ui_thread: PhantomData<*const ()>,
}

impl InputMonitor {
    pub fn request_stop(&self) {
        self.native.request_stop();
    }

    pub fn stopped(&self) -> impl Future<Output = ()> + use<> {
        let observation = self.thread.exited();
        let native = self.native.stopped();
        async move {
            observation.await;
            native.await;
        }
    }
}

impl fmt::Debug for InputMonitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputMonitor").finish_non_exhaustive()
    }
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.request_stop();
    }
}

pub fn prepare(
    input: InputSender,
    options: DesktopOptions,
    own_focus: OwnFocus,
) -> anyhow::Result<(InputMonitor, Inserter)> {
    let (thread, native, inserter) = native::start(input, options, own_focus)?;
    let monitor = InputMonitor {
        thread,
        native,
        _ui_thread: PhantomData,
    };
    Ok((monitor, inserter))
}

#[derive(Clone, Debug)]
pub struct DesktopOptions {
    pub shortcut_key: Shortcut,
    pub shortcut: String,
    pub cancel: String,
    pub paste: LinuxPaste,
    pub external_shortcut: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacShortcut {
    #[default]
    Function,
    RightOption,
    RightCommand,
    F13,
}

impl MacShortcut {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Function => "Fn",
            Self::RightOption => "Right Option",
            Self::RightCommand => "Right Command",
            Self::F13 => "F13",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shortcut {
    pub mac: MacShortcut,
    pub windows: WindowsShortcut,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowsShortcut {
    #[default]
    ControlWindows,
    RightControl,
    F13,
}

impl WindowsShortcut {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ControlWindows => "Ctrl + Win",
            Self::RightControl => "Right Ctrl",
            Self::F13 => "F13",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxPaste {
    Standard,
    Terminal,
    Manual,
}

impl Default for DesktopOptions {
    fn default() -> Self {
        Self {
            shortcut_key: Shortcut::default(),
            shortcut: "CTRL+LOGO+space".into(),
            cancel: "CTRL+LOGO+Escape".into(),
            paste: LinuxPaste::Standard,
            external_shortcut: false,
        }
    }
}

pub struct NativeWindow {
    #[cfg(not(target_os = "linux"))]
    pub(crate) native: native::Window,
    /// Keeps the handle `!Send` everywhere, so presentation stays on the window's thread.
    _ui_thread: PhantomData<*const ()>,
}

impl NativeWindow {
    pub fn new(handle: WindowHandle<'_>) -> anyhow::Result<Self> {
        #[cfg(target_os = "linux")]
        native::check_window(handle)?;
        Ok(Self {
            #[cfg(not(target_os = "linux"))]
            native: native::Window::new(handle)?,
            _ui_thread: PhantomData,
        })
    }
}

impl fmt::Debug for NativeWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeWindow").finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FileFilter<'a> {
    pub name: &'a str,
    pub pattern: &'a str,
}

#[must_use]
pub fn reduced_motion() -> bool {
    native::reduced_motion()
}

/// Whether secure keyboard entry is on; call on the main thread, where macOS can answer.
///
/// Elsewhere macOS answers `false`, and Windows and Linux always do.
#[must_use]
pub fn secure_input() -> bool {
    native::secure_input()
}

/// Call off the UI thread; Windows retries for up to 50 ms while the clipboard is busy.
pub fn copy_text(text: &str) -> anyhow::Result<()> {
    native::copy_text(text)
}

/// Call only for a command or a run about to end.
///
/// Windows gives a windowed executable no console handles, so this attaches the parent's console,
/// and that console's Ctrl+C then reaches this process too.
#[expect(
    clippy::must_use_candidate,
    reason = "Attaching is the point; only a caller with a dialog to fall back on needs the answer"
)]
pub fn attach_stdio() -> bool {
    native::attach_stdio()
}

/// Call on the main thread, outside any GPUI update.
///
/// macOS shows nothing off the main thread, and the modal loop runs queued GPUI tasks, which
/// panic if the app is already borrowed.
pub fn show_startup_error(message: &str) {
    native::show_startup_error(message);
}
