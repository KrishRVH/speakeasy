//! The desktop facade every platform shares, declared and documented once over each OS's native
//! module.

use std::marker::PhantomData;

use raw_window_handle::WindowHandle;

use crate::native;

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
