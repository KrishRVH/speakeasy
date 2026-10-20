//! Clipboard paste and direct text input on Windows.

use std::{
    num::NonZeroU32,
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    System::Threading::GetCurrentProcessId,
    UI::{
        Input::KeyboardAndMouse::{INPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VK_CONTROL, VK_V},
        WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    },
};

use super::{modifiers, send_input, synthetic_key};
use crate::{Inserted, insertion::NativeInsert};

pub(crate) fn copy_text(text: &str) -> anyhow::Result<()> {
    let text = windows_line_endings(text);
    let _clipboard = open_clipboard()?;
    clipboard_win::raw::set_string(&text)?;
    Ok(())
}

/// The native steps of one insertion, which [`crate::insertion::insert`] puts in order.
pub(crate) struct Insert;

impl Insert {
    pub(crate) const fn new() -> Self {
        Self
    }
}

/// Keyboard input built in full, submitted only after the recording commits.
pub(crate) enum Keys {
    /// Ctrl+V.
    Paste([INPUT; 4]),
    /// A press and release for each UTF-16 unit of the text.
    Text(Vec<INPUT>),
}

impl NativeInsert for Insert {
    type Keys = Keys;
    type Lease = clipboard_win::Clipboard;
    /// The clipboard sequence number, which every change advances.
    type Mark = Option<NonZeroU32>;

    fn acquire(&mut self) -> anyhow::Result<clipboard_win::Clipboard> {
        open_clipboard()
    }

    fn write(&mut self, lease: clipboard_win::Clipboard, text: &str) -> anyhow::Result<()> {
        clipboard_win::raw::set_string(&windows_line_endings(text))?;
        drop(lease);
        Ok(())
    }

    fn mark(&mut self) -> Option<NonZeroU32> {
        clipboard_win::raw::seq_num()
    }

    fn holds(&mut self, text: &str, mark: &Option<NonZeroU32>) -> anyhow::Result<bool> {
        let _clipboard = open_clipboard()?;
        let mut current = Vec::new();
        clipboard_win::raw::get_string(&mut current)?;
        Ok(current == windows_line_endings(text).as_bytes()
            && *mark == clipboard_win::raw::seq_num())
    }

    fn paste_keys(&mut self) -> anyhow::Result<Keys> {
        let press = |key| synthetic_key(key, 0, 0);
        let release = |key| synthetic_key(key, 0, KEYEVENTF_KEYUP);
        Ok(Keys::Paste([
            press(VK_CONTROL),
            press(VK_V),
            release(VK_V),
            release(VK_CONTROL),
        ]))
    }

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Keys> {
        Ok(Keys::Text(
            text.replace("\r\n", "\n")
                .encode_utf16()
                .flat_map(|unit| {
                    [
                        synthetic_key(0, unit, KEYEVENTF_UNICODE),
                        synthetic_key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                    ]
                })
                .collect(),
        ))
    }

    fn external_focus(&mut self) -> bool {
        has_external_target()
    }

    fn shortcut_released(&mut self) -> bool {
        !modifiers().any()
    }

    fn send(&mut self, keys: Keys) -> anyhow::Result<Inserted> {
        match keys {
            Keys::Paste(shortcut) => {
                let sent = send_input(&shortcut)?;
                if sent == shortcut.len() {
                    return Ok(Inserted::Sent);
                }
                if sent > 0 {
                    let release = |key| synthetic_key(key, 0, KEYEVENTF_KEYUP);
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "Release stuck synthetic modifiers after partial input even if the target also blocks cleanup"
                    )]
                    let _ = send_input(&[release(VK_V), release(VK_CONTROL)]);
                }
                Ok(Inserted::Copied(
                    "Copied. This app blocked paste; press Ctrl+V.",
                ))
            },
            Keys::Text(input) => {
                let sent = send_input(&input)?;
                if sent != input.len() {
                    return Ok(Inserted::Unavailable(
                        "Direct input was blocked or only partly sent. Clipboard preserved.",
                    ));
                }
                Ok(Inserted::Sent)
            },
        }
    }
}

fn windows_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Opens the clipboard, retrying for a short wall-clock budget because clipboard listeners can hold
/// it right after a change. Runs on the insertion worker, never the UI thread.
fn open_clipboard() -> anyhow::Result<clipboard_win::Clipboard> {
    let started = Instant::now();
    let until = started
        .checked_add(Duration::from_millis(50))
        .unwrap_or(started);
    loop {
        match clipboard_win::Clipboard::new() {
            Ok(clipboard) => return Ok(clipboard),
            Err(error) if Instant::now() >= until => return Err(error.into()),
            Err(_) => thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn has_external_target() -> bool {
    // SAFETY: foreground HWND is queried and used immediately, with a valid PID out parameter.
    unsafe {
        let window = GetForegroundWindow();
        let mut pid = 0;
        !window.is_null()
            && GetWindowThreadProcessId(window, &raw mut pid) != 0
            && pid != GetCurrentProcessId()
    }
}
