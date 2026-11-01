mod dialog;
mod input;
mod insertion;
pub(crate) mod installation;
pub(crate) mod login;
pub(crate) mod movie;
pub(crate) use movie::read::frame as read_movie_frame;
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
    insertion::{Insert, copy_content, copy_text},
};
#[cfg(test)]
use crate::NativeWindow;
use crate::keyboard::{self, Modifiers};

const OWN_INPUT: usize = 0x5350_4541;

const BASE_DPI: u32 = 96;

fn is_key_down(key: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

pub(crate) fn modifiers() -> Modifiers {
    keyboard::Windows::modifiers(is_key_down)
}

pub(crate) fn reduced_motion() -> bool {
    let mut animates = TRUE;

    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animates).cast(), 0);
    }
    animates == FALSE
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
        unsafe {
            let style = GetWindowLongW(self.hwnd, index).cast_unsigned();
            SetWindowLongW(self.hwnd, index, change(style).cast_signed());
        }
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

#[cfg(test)]
unsafe fn native_window(hwnd: HWND) -> anyhow::Result<NativeWindow> {
    let raw = std::num::NonZeroIsize::new(hwnd as isize).context("Missing window")?;
    let raw = RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(raw));

    NativeWindow::new(unsafe { WindowHandle::borrow_raw(raw) })
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

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
