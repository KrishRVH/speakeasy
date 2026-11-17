pub(crate) mod awake;
mod dialog;
#[cfg(test)]
mod fixture;
mod input;
mod insertion;
pub(crate) mod installation;
pub(crate) mod login;
pub(crate) mod movie;
mod presentation;
pub(crate) mod registry;
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
    insertion::{copy_content, copy_text},
};
use crate::keyboard::Modifiers;

const OWN_INPUT: usize = 0x5350_4541;

const BASE_DPI: u32 = 96;

fn is_key_down(key: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

fn modifiers(shortcut: crate::WindowsShortcut) -> Modifiers {
    shortcut.modifiers(is_key_down)
}

pub(crate) fn reduced_motion() -> bool {
    let mut animates = TRUE;

    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animates).cast(), 0);
    }
    animates == FALSE
}

pub(crate) fn secure_input() -> bool {
    false
}

pub(crate) fn attach_stdio() -> bool {
    let missing = |stream| {
        let handle = unsafe { GetStdHandle(stream) };
        handle.is_null() || handle == INVALID_HANDLE_VALUE
    };
    if missing(STD_OUTPUT_HANDLE) || missing(STD_ERROR_HANDLE) {
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
    !missing(STD_ERROR_HANDLE)
}

pub(crate) fn show_startup_error(message: &str) {
    let text = to_wide(message);

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

    let sent = unsafe { SendInput(count, input.as_ptr(), size) };
    Ok(usize::try_from(sent)?)
}

pub(crate) unsafe fn take_handle(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }

    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

pub(crate) fn ok_or_last_error(result: BOOL) -> io::Result<()> {
    if result == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) struct Window {
    hwnd: HWND,
}

impl Window {
    pub(crate) fn new(handle: WindowHandle<'_>) -> anyhow::Result<Self> {
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            bail!("Expected a Windows window");
        };
        let hwnd = handle.hwnd.get() as HWND;

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

    fn restyle(&self, index: WINDOW_LONG_PTR_INDEX, change: impl FnOnce(u32) -> u32) {
        let style = change(unsafe { GetWindowLongW(self.hwnd, index) }.cast_unsigned());

        unsafe { SetWindowLongW(self.hwnd, index, style.cast_signed()) };
    }
}

fn window_process(window: HWND) -> Option<u32> {
    let mut process = 0;

    let thread = unsafe { GetWindowThreadProcessId(window, &raw mut process) };
    (thread != 0).then_some(process)
}

fn is_own_window(window: HWND) -> bool {
    window_process(window) == Some(std::process::id())
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::fixture::{native_window, with_foreign_window};

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn a_window_another_thread_owns_is_refused() -> anyhow::Result<()> {
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
