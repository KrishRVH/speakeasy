//! Windows adapter: the Ctrl+Win keyboard hook, session-change cancellation, GPUI window
//! presentation, the file dialog, and clipboard or direct text insertion.
//!
//! This module keeps what its children share: synthetic input tagged so the hook ignores it,
//! and handle and string conversions.

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

use anyhow::{Context as _, bail};
use raw_window_handle::RawWindowHandle;
use windows_sys::{
    Win32::{
        Foundation::{FALSE, HANDLE, HWND, TRUE},
        System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
            },
            WindowsAndMessaging::{
                MB_ICONERROR, MB_OK, MessageBoxW, SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
            },
        },
    },
    core::BOOL,
    w,
};

pub use self::{
    dialog::choose_file,
    insertion::copy_text,
    presentation::{configure_pill, minimize_to_tray, set_pill_visible, set_settings_visible},
};
pub(crate) use self::{input::HookMonitor, insertion::Insert};

/// Tags input Speakeasy synthesizes, so its own keyboard hook ignores it.
const OWN_INPUT: usize = 0x5350_4541;
/// The DPI at which one device-independent pixel is one physical pixel.
const BASE_DPI: u32 = 96;

fn is_key_down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: GetAsyncKeyState takes a key code by value and retains nothing.
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

pub(super) fn modifiers_down() -> bool {
    [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
        .into_iter()
        .any(is_key_down)
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

fn hwnd_of(handle: RawWindowHandle) -> anyhow::Result<HWND> {
    match handle {
        RawWindowHandle::Win32(window) => Ok(window.hwnd.get() as HWND),
        _ => bail!("Expected a Windows window"),
    }
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
