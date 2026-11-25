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

use anyhow::{Context as _, bail};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use windows_sys::{
    Win32::{
        Foundation::{FALSE, HANDLE, HWND, INVALID_HANDLE_VALUE, TRUE},
        System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_CHAR, MapVirtualKeyW, SendInput,
                VIRTUAL_KEY,
            },
            WindowsAndMessaging::{
                GetClassNameW, GetWindowLongW, GetWindowThreadProcessId, MB_ICONERROR, MB_OK,
                MessageBoxW, SPI_GETCLIENTAREAANIMATION, SetWindowLongW, SystemParametersInfoW,
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

/// Tags input Speakeasy synthesizes so its own keyboard hook ignores it.
const OWN_INPUT: usize = 0x5350_4541;

const BASE_DPI: u32 = 96;

/// The shell's desktop window classes.
const DESKTOP_CLASSES: [&str; 2] = ["Progman", "WorkerW"];

/// The shell's taskbar window classes, primary and secondary.
const BAR_CLASSES: [&str; 2] = ["Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

fn is_key_down(key: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

/// The character the calling thread's keyboard layout prints on the key with virtual-key `code`.
pub(crate) fn printed_key(code: u16) -> Option<char> {
    // SAFETY: MapVirtualKeyW only reads the calling thread's keyboard layout.
    let mapped = unsafe { MapVirtualKeyW(u32::from(code), MAPVK_VK_TO_CHAR) };
    // The top bit marks a dead key, whose low word still holds the character it prints.
    char::from_u32(mapped & 0xFFFF)
        .filter(|character| !character.is_control() && !character.is_whitespace())
}

pub(crate) fn reduced_motion() -> bool {
    let mut animates = TRUE;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL through the pointer, which outlives the call.
    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animates).cast(), 0);
    }
    animates == FALSE
}

pub(crate) const fn secure_input() -> Option<crate::SecureInput> {
    None
}

pub(crate) const fn globe_key() -> Option<crate::GlobeKey> {
    None
}

pub(crate) fn attach_stdio() -> bool {
    let missing = |stream| {
        let handle = unsafe { GetStdHandle(stream) };
        handle.is_null() || handle == INVALID_HANDLE_VALUE
    };
    if missing(STD_OUTPUT_HANDLE) || missing(STD_ERROR_HANDLE) {
        // Attaching fills only the missing handles, so a redirected stream keeps its target.
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

/// Adopts a fresh kernel handle, or reports the last error for a null one.
///
/// # Safety
/// `raw` comes straight from a Win32 call that fails with null, not `INVALID_HANDLE_VALUE`,
/// and otherwise returns a handle closed by `CloseHandle` that nothing else owns.
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

/// A borrowed HWND, used only on the thread that owns it.
///
/// Subclass and presentation calls need the owning thread, and `new` does not check it:
/// `NativeWindow` is built on GPUI's UI thread and is `!Send`.
pub(crate) struct Window {
    hwnd: HWND,
}

impl Window {
    pub(crate) fn new(handle: WindowHandle<'_>) -> anyhow::Result<Self> {
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            bail!("Expected a Windows window");
        };
        Ok(Self {
            hwnd: handle.hwnd.get() as HWND,
        })
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

fn class_name(window: HWND) -> String {
    let mut class = [0_u16; 256];

    let length = unsafe {
        GetClassNameW(
            window,
            class.as_mut_ptr(),
            i32::try_from(class.len()).unwrap_or_default(),
        )
    };
    let length = usize::try_from(length).unwrap_or_default();
    String::from_utf16_lossy(class.get(..length).unwrap_or_default())
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
