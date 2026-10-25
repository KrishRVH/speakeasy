//! The desktop facade every platform shares, declared and documented once over each OS's native
//! module: input observation, system services, and the `NativeWindow` handle. The handle's
//! presentation methods live beside the native code behind them: in each OS's module, and in
//! `screen` for the Show overlay.

use std::marker::PhantomData;

use raw_window_handle::WindowHandle;

use crate::{DesktopOptions, InputSender, Inserter, OwnedThread, native};

/// Owns desktop observation, and on Windows and macOS the insertion thread, until an explicit stop
/// and acknowledged cleanup.
///
/// Create, stop, and drop it on the UI thread. Dropping it requests a stop and then joins its
/// threads; await [`stopped`](Self::stopped) first so the join does not block.
pub struct InputMonitor {
    /// The observation thread. Declared first, so dropping joins it before `native` releases the
    /// rest: macOS removes its lifecycle observers only once the event tap's thread has ended.
    thread: OwnedThread,
    native: native::Monitor,
    /// Not `Send` even where the native monitor would be, so code checked on one host keeps the
    /// rule macOS needs: its lifecycle observers are not `Send`.
    _ui_thread: PhantomData<*const ()>,
}

impl InputMonitor {
    /// Requests that observation and insertion stop, without joining their threads.
    pub fn request_stop(&self) {
        self.native.request_stop();
    }

    /// Resolves once every thread has retired its native resources; await it before replacing or
    /// dropping the monitor.
    pub fn stopped(&self) -> impl Future<Output = ()> + use<> {
        let observation = self.thread.exited();
        let native = self.native.stopped();
        async move {
            observation.await;
            native.await;
        }
    }
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Starts desktop observation and the insertion that serves the returned [`Inserter`].
///
/// Readiness arrives on `input` as [`Input::DesktopReady`](crate::Input::DesktopReady), and a later
/// loss of desktop access, such as a missing permission, as
/// [`Input::Unavailable`](crate::Input::Unavailable).
///
/// # Errors
/// Returns thread startup or native desktop initialization failures.
pub fn prepare(
    input: InputSender,
    options: DesktopOptions,
) -> anyhow::Result<(InputMonitor, Inserter)> {
    let (thread, native, inserter) = native::start(input, options)?;
    let monitor = InputMonitor {
        thread,
        native,
        _ui_thread: PhantomData,
    };
    Ok((monitor, inserter))
}

/// A live window of this process, for the native presentation GPUI does not offer.
///
/// Build it from the handle GPUI lends for an open window and use it before that window closes.
/// It is not `Send` on any platform, so it stays on the UI thread that owns the window: Windows
/// refuses a window another thread owns, and macOS requires the main thread. Showing, hiding, and
/// resizing can call back into GPUI synchronously, so call those methods with GPUI's borrows
/// released.
pub struct NativeWindow {
    pub(crate) native: native::Window,
    /// Not `Send` even where the native window would be, so code checked on one host keeps the
    /// thread rule every platform needs.
    _ui_thread: PhantomData<*const ()>,
}

impl NativeWindow {
    /// The window behind `handle`, a window of this process.
    ///
    /// # Errors
    /// Fails for a handle of another windowing system, or one this thread may not use: on Windows a
    /// window another thread owns, on macOS any thread but the main one.
    pub fn new(handle: WindowHandle<'_>) -> anyhow::Result<Self> {
        Ok(Self {
            native: native::Window::new(handle)?,
            _ui_thread: PhantomData,
        })
    }
}

/// Whether the system asks apps to reduce motion; any thread may ask.
///
/// Linux reads no desktop preference and answers `false`, leaving the choice to the app setting.
#[must_use]
pub fn reduced_motion() -> bool {
    native::reduced_motion()
}

/// Places `text` on the clipboard, with CRLF line endings on Windows.
///
/// Call it off the UI thread: Windows retries for up to 50 ms while another app holds the clipboard.
///
/// # Errors
/// Fails when another app holds the clipboard or refuses the text, and always on Linux, which
/// records no Shows.
pub fn copy_text(text: &str) -> anyhow::Result<()> {
    native::copy_text(text)
}

/// Presents an error that prevents or ends a launch: a dialog that blocks until dismissed on
/// Windows and macOS, a line on stderr on Linux.
///
/// Call it on the main thread, where macOS presents its alert; elsewhere macOS shows nothing. The
/// app reports runtime failures in its own windows instead. Callers never include audio,
/// transcripts, or credentials.
pub fn show_startup_error(message: &str) {
    native::show_startup_error(message);
}
