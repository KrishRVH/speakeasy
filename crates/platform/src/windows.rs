//! Windows adapter: the Ctrl+Win keyboard hook and the session changes that interrupt a recording,
//! clipboard or direct text insertion, GPUI window presentation, the file dialog, Show screen
//! capture, and the sign-in launch.
//!
//! This module serves `desktop`'s reduced-motion query, launching-console attachment, and startup
//! error dialog, and keeps what the Windows code shares: held modifiers, synthetic input tagged so
//! the hook ignores it, the thread-checked window behind `NativeWindow`, which process owns a
//! window, and handle and string conversions.

mod dialog;
mod input;
mod insertion;
pub(crate) mod login;
mod presentation;
pub(crate) mod screen;

use std::{
    io,
    os::windows::io::{FromRawHandle, OwnedHandle},
    ptr::null_mut,
};

use anyhow::{Context as _, bail, ensure};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use windows_sys::{
    Win32::{
        Foundation::{FALSE, HANDLE, HWND, INVALID_HANDLE_VALUE, TRUE},
        System::{
            Console::{
                ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
                STD_OUTPUT_HANDLE,
            },
            Threading::GetCurrentThreadId,
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY,
            },
            WindowsAndMessaging::{
                GetWindowLongW, GetWindowThreadProcessId, MB_ICONERROR, MB_OK, MessageBoxW,
                SPI_GETCLIENTAREAANIMATION, SetWindowLongW, SystemParametersInfoW,
                WINDOW_LONG_PTR_INDEX,
            },
        },
    },
    core::BOOL,
    w,
};

pub(crate) use self::{
    input::{Monitor, start},
    insertion::{Insert, copy_text},
};
#[cfg(test)]
use crate::NativeWindow;
use crate::keyboard::{self, Modifiers};

/// Tags input Speakeasy synthesizes, so its own keyboard hook ignores it.
const OWN_INPUT: usize = 0x5350_4541;
/// The DPI at which one device-independent pixel is one physical pixel.
const BASE_DPI: u32 = 96;

fn is_key_down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: GetAsyncKeyState takes a key code by value and retains nothing.
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

/// The modifier keys physically held now.
pub(crate) fn modifiers() -> Modifiers {
    keyboard::Windows::modifiers(is_key_down)
}

pub(crate) fn reduced_motion() -> bool {
    let mut animates = TRUE;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes a BOOL into this live variable.
    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animates).cast(), 0);
    }
    animates == FALSE
}

pub(crate) fn attach_stdio() -> bool {
    let missing = |stream| {
        // SAFETY: GetStdHandle takes no pointers.
        let handle = unsafe { GetStdHandle(stream) };
        handle.is_null() || handle == INVALID_HANDLE_VALUE
    };
    if missing(STD_OUTPUT_HANDLE) || missing(STD_ERROR_HANDLE) {
        // Attaching fills in only the missing handles, so a redirected stream keeps its destination.
        // SAFETY: AttachConsole takes no pointers; a failed attach changes nothing.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
    !missing(STD_ERROR_HANDLE)
}

pub(crate) fn show_startup_error(message: &str) {
    let text = to_wide(message);
    // SAFETY: both NUL-terminated strings outlive this synchronous dialog.
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            w!("Speakeasy"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn press(key: VIRTUAL_KEY) -> INPUT {
    synthetic_key(key, 0, 0)
}

fn release(key: VIRTUAL_KEY) -> INPUT {
    synthetic_key(key, 0, KEYEVENTF_KEYUP)
}

/// A press and release that types one UTF-16 code unit, whatever the keyboard layout.
fn type_unit(unit: u16) -> [INPUT; 2] {
    [
        synthetic_key(0, unit, KEYEVENTF_UNICODE),
        synthetic_key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
    ]
}

fn synthetic_key(key: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: OWN_INPUT,
            },
        },
    }
}

fn send_input(input: &[INPUT]) -> anyhow::Result<usize> {
    let count = u32::try_from(input.len()).context("Text is too long for native keyboard input")?;
    let size = i32::try_from(size_of::<INPUT>())?;
    // SAFETY: initialized keyboard records and their buffer outlive this synchronous submission;
    // both lengths are checked against the native ABI.
    let sent = unsafe { SendInput(count, input.as_ptr(), size) };
    Ok(usize::try_from(sent)?)
}

/// Takes ownership of a handle a Win32 call just returned, or reports its failure.
///
/// # Safety
/// `raw` must come straight from a Win32 call that returns null on failure and otherwise an open
/// kernel-object handle, released with `CloseHandle`, that nothing else owns.
pub(crate) unsafe fn take_handle(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the caller passes an open, unowned kernel-object handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Converts a Win32 `BOOL` result, reading the thread's last error on failure.
pub(crate) fn ok_or_last_error(result: BOOL) -> io::Result<()> {
    if result == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A window this thread owns. Subclasses, which the pill, Settings, and the overlay install, work
/// only on the owning thread, and checking once here keeps every later call on it.
pub(crate) struct Window {
    hwnd: HWND,
}

impl Window {
    pub(crate) fn new(handle: WindowHandle<'_>) -> anyhow::Result<Self> {
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            bail!("Expected a Windows window");
        };
        let hwnd = handle.hwnd.get() as HWND;
        // SAFETY: the borrowed handle names a live window, and a null out-parameter skips the
        // process id.
        let (owner, current) = unsafe {
            (
                GetWindowThreadProcessId(hwnd, null_mut()),
                GetCurrentThreadId(),
            )
        };
        ensure!(owner != 0, "The window no longer exists");
        ensure!(owner == current, "Another thread owns this window");
        Ok(Self { hwnd })
    }

    /// Rewrites the window's 32-bit `GWL_STYLE` or `GWL_EXSTYLE` bits through `change`.
    fn restyle(&self, index: WINDOW_LONG_PTR_INDEX, change: impl FnOnce(u32) -> u32) {
        // SAFETY: `new` checked that this thread owns the window, and both calls take values only.
        unsafe {
            let style = GetWindowLongW(self.hwnd, index).cast_unsigned();
            SetWindowLongW(self.hwnd, index, change(style).cast_signed());
        }
    }
}

/// The process that owns `window`, or `None` for a handle that names no window.
fn window_process(window: HWND) -> Option<u32> {
    let mut process = 0;
    // SAFETY: writes only the live local; a null or stale handle reports no thread.
    let thread = unsafe { GetWindowThreadProcessId(window, &raw mut process) };
    (thread != 0).then_some(process)
}

/// Whether `window` belongs to this process.
fn is_own_window(window: HWND) -> bool {
    window_process(window) == Some(std::process::id())
}

/// The `NativeWindow` for a window a test created.
///
/// # Safety
/// `hwnd` must name a live window.
#[cfg(test)]
unsafe fn native_window(hwnd: HWND) -> anyhow::Result<NativeWindow> {
    let raw = std::num::NonZeroIsize::new(hwnd as isize).context("Missing window")?;
    let raw = RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(raw));
    // SAFETY: the caller passes a live window, which outlives this borrow.
    NativeWindow::new(unsafe { WindowHandle::borrow_raw(raw) })
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Creates a message-only window on a thread of its own and runs `on_owner` there, then runs
/// `elsewhere` on this thread; the window stays live until both have returned.
#[cfg(test)]
fn with_foreign_window<O: Send + 'static, T>(
    on_owner: impl FnOnce(HWND) -> O + Send + 'static,
    elsewhere: impl FnOnce(HWND) -> T,
) -> anyhow::Result<(O, T)> {
    use std::{ptr::null, sync::mpsc, thread};

    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE},
    };

    let (created, window) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let owner = thread::spawn(move || -> anyhow::Result<()> {
        // SAFETY: STATIC is a system class, every pointer argument is static or null, and this
        // thread destroys the message-only window below.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        ensure!(!hwnd.is_null(), "Cannot create the test window");
        if created.send((hwnd as isize, on_owner(hwnd))).is_ok() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A closed lane means the calling thread is gone, so the window can go too"
            )]
            let _ = released.recv();
        }
        // SAFETY: this thread created the window and destroys it once.
        unsafe { DestroyWindow(hwnd) };
        Ok(())
    });
    let (hwnd, from_owner) = window.recv()?;
    let from_here = elsewhere(hwnd as HWND);
    release.send(())?;
    owner.join().expect("The window's owner thread panicked")?;
    Ok((from_owner, from_here))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn a_window_another_thread_owns_is_refused() -> anyhow::Result<()> {
        // SAFETY: `with_foreign_window` keeps the window live while either check runs.
        let accepted = |hwnd| unsafe { native_window(hwnd) }.is_ok();
        let (accepted_on_owner, accepted_elsewhere) = with_foreign_window(accepted, accepted)?;
        assert!(
            accepted_on_owner,
            "The owning thread could not use its own window"
        );
        assert!(
            !accepted_elsewhere,
            "A window another thread owns was accepted"
        );
        Ok(())
    }
}
