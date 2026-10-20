//! Windows adapter: the Ctrl+Win keyboard hook, session-change cancellation, GPUI window
//! presentation, the file dialog, and clipboard or direct text insertion.
//!
//! This module keeps what its children share: synthetic input tagged so the hook ignores it, the
//! thread-checked window behind `NativeWindow`, and handle and string conversions.

mod dialog;
mod input;
mod insertion;
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
        Foundation::{FALSE, HANDLE, HWND, TRUE},
        System::{
            Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE},
            Threading::GetCurrentThreadId,
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
            },
            WindowsAndMessaging::{
                GetWindowThreadProcessId, MB_ICONERROR, MB_OK, MessageBoxW,
                SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
            },
        },
    },
    core::BOOL,
    w,
};

pub use self::insertion::copy_text;
pub(crate) use self::{input::HookMonitor, insertion::Insert};
#[cfg(test)]
use crate::NativeWindow;
use crate::keyboard::Modifiers;

/// Tags input Speakeasy synthesizes, so its own keyboard hook ignores it.
const OWN_INPUT: usize = 0x5350_4541;
/// The DPI at which one device-independent pixel is one physical pixel.
const BASE_DPI: u32 = 96;

fn is_key_down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: GetAsyncKeyState takes a key code by value and retains nothing.
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

/// The modifier keys physically held now.
pub(super) fn modifiers() -> Modifiers {
    Modifiers {
        shift: is_key_down(VK_SHIFT),
        shortcut: [VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .into_iter()
            .any(is_key_down),
        function: false,
    }
}

/// Returns whether the system asks apps to reduce motion; Linux uses the app setting.
#[must_use]
pub fn reduced_motion() -> bool {
    let mut animates = TRUE;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes a BOOL into this live variable.
    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animates).cast(), 0);
    }
    animates == FALSE
}

/// Sends output to the terminal that launched this windowed executable, if one did.
///
/// A windowed executable starts without a console, so command-line output would vanish. Output
/// already redirected to a pipe or file keeps its destination.
pub fn attach_console() {
    // SAFETY: neither call takes pointers; a failed attach leaves output where it was.
    unsafe {
        if GetStdHandle(STD_OUTPUT_HANDLE).is_null() {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Presents an error that prevents or ends a launch, blocking until the user dismisses it.
///
/// The app reports runtime failures in its own windows instead. Callers never include audio,
/// transcripts, or credentials.
pub fn show_startup_error(message: &str) {
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

fn synthetic_key(key: VIRTUAL_KEY, unit: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: unit,
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
}

/// The token for a window a test created.
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

#[cfg(test)]
mod tests {
    use std::{ptr::null, sync::mpsc, thread};

    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE},
    };

    use super::*;

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn a_window_another_thread_owns_is_refused() -> anyhow::Result<()> {
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
            // SAFETY: the window stays live until this thread destroys it below.
            let accepted_here = unsafe { native_window(hwnd) }.is_ok();
            created.send((hwnd as isize, accepted_here))?;
            // An error means the test thread is gone, so the window can go too.
            let _released = released.recv();
            // SAFETY: this thread created the window and destroys it once.
            unsafe { DestroyWindow(hwnd) };
            Ok(())
        });
        let (hwnd, accepted_on_owner) = window.recv()?;
        // SAFETY: the owner thread keeps the window live until it is released below.
        let accepted_elsewhere = unsafe { native_window(hwnd as HWND) }.is_ok();
        release.send(())?;
        owner.join().expect("The window's owner thread panicked")?;
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
