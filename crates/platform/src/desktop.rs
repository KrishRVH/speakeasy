use std::{fmt, marker::PhantomData};

use raw_window_handle::WindowHandle;

use crate::{InputSender, Inserter, OwnFocus, OwnedThread, native};

pub struct InputMonitor {
    thread: OwnedThread,
    native: native::Monitor,
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
    pub mac_shortcut: MacShortcut,
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
}

impl MacShortcut {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Function => "Fn",
            Self::RightOption => "Right Option",
            Self::RightCommand => "Right Command",
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
            mac_shortcut: MacShortcut::default(),
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

pub fn copy_text(text: &str) -> anyhow::Result<()> {
    native::copy_text(text)
}

#[expect(
    clippy::must_use_candidate,
    reason = "Attaching is the point; only a caller with a dialog to fall back on needs the answer"
)]
pub fn attach_stdio() -> bool {
    native::attach_stdio()
}

pub fn show_startup_error(message: &str) {
    native::show_startup_error(message);
}
