#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::sync::Arc;
use std::{fmt, marker::PhantomData};

use raw_window_handle::WindowHandle;

use crate::{
    InputSender, Inserter, MacShortcut, OwnFocus, OwnedThread, Shortcuts, WindowsShortcut, native,
};

/// Owns desktop observation and, on Windows and macOS, the insertion thread.
///
/// Create, stop, and drop it on the UI thread. Drop joins its threads; await `stopped` first so
/// the join does not block.
pub struct InputMonitor {
    /// Declared first: drop joins it before `native` removes the macOS lifecycle observers.
    thread: OwnedThread,
    native: native::Monitor,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    recording: Arc<crate::keyboard::Recording>,
    /// Keeps the monitor `!Send` on every host, as the macOS lifecycle observers require.
    _ui_thread: PhantomData<*const ()>,
}

impl InputMonitor {
    pub fn request_stop(&self) {
        self.native.request_stop();
    }

    /// Records the next shortcut pressed instead of acting on any.
    ///
    /// The receiver reports the keys held, then the result, and closes when the recording ends.
    /// Dropping it, or starting another, ends the recording.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[must_use]
    pub fn record_shortcut(&self) -> async_channel::Receiver<crate::Recorded> {
        self.recording.start()
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
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let recording = Arc::default();
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let (thread, native, inserter) =
        native::start(input, options, own_focus, Arc::clone(&recording))?;
    #[cfg(target_os = "linux")]
    let (thread, native, inserter) = native::start(input, options, own_focus)?;
    let monitor = InputMonitor {
        thread,
        native,
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        recording,
        _ui_thread: PhantomData,
    };
    Ok((monitor, inserter))
}

#[derive(Clone, Debug)]
pub struct DesktopOptions {
    pub mac: Shortcuts<MacShortcut>,
    pub windows: Shortcuts<WindowsShortcut>,
    pub shortcut: String,
    pub cancel: String,
    pub paste: LinuxPaste,
    pub external_shortcut: bool,
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
            mac: Shortcuts::default(),
            windows: Shortcuts::default(),
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

/// What macOS does with a tap of the Fn (🌐) key besides passing it on, as Keyboard settings
/// choose under "Press 🌐 key to".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobeKey {
    InputSource,
    Emoji,
    Dictation,
}

/// The action macOS gives a tap of the Fn (🌐) key; `None` for Do Nothing and off macOS.
///
/// A Mac that never saved the setting also answers `None`, since its default varies by model.
#[must_use]
pub fn globe_key() -> Option<GlobeKey> {
    native::globe_key()
}

/// Secure keyboard entry, during which macOS shows Speakeasy only modifier keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecureInput {
    /// The app macOS names as turning it on, if it names one.
    ///
    /// macOS can name the frontmost app while a background app holds it.
    pub holder: Option<String>,
}

/// Secure keyboard entry, if it is on; call on the main thread, where macOS can answer.
///
/// Elsewhere macOS answers `None`, and Windows and Linux always do.
#[must_use]
pub fn secure_input() -> Option<SecureInput> {
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
