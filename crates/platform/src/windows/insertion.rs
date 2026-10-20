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

use super::{modifiers_down, send_input, synthetic_key};
use crate::{Delivery, InsertPermit, Inserted};

/// Places `text` on the clipboard, with Windows line endings.
///
/// # Errors
///
/// Fails when another application holds the clipboard or refuses the text.
pub fn copy_text(text: &str) -> anyhow::Result<()> {
    let text = text.replace("\r\n", "\n").replace('\n', "\r\n");
    let _clipboard = open_clipboard()?;
    clipboard_win::raw::set_string(&text)?;
    Ok(())
}

pub(crate) fn insert(
    text: &str,
    permit: &InsertPermit,
    delivery: Delivery,
) -> anyhow::Result<Inserted> {
    if !permit.active() {
        return Ok(Inserted::Cancelled);
    }
    match delivery {
        Delivery::Paste => paste(text, permit),
        Delivery::Direct => insert_direct(text, permit),
    }
}

fn paste(text: &str, permit: &InsertPermit) -> anyhow::Result<Inserted> {
    let text = text.replace("\r\n", "\n").replace('\n', "\r\n");
    {
        let _clipboard = open_clipboard()?;
        if !permit.active() {
            return Ok(Inserted::Cancelled);
        }
        clipboard_win::raw::set_string(&text)?;
    }
    let sequence = clipboard_win::raw::seq_num();
    if !clipboard_holds(&text, sequence)? {
        return Ok(Inserted::Unavailable(
            "Clipboard changed. New contents were preserved; paste was not sent.",
        ));
    }
    if let Some(outcome) = Delivery::Paste.preflight(has_external_target(), !modifiers_down()) {
        return Ok(outcome);
    }
    if sequence != clipboard_win::raw::seq_num() {
        return Ok(Inserted::Unavailable(
            "Clipboard changed before paste. New contents were preserved.",
        ));
    }
    if !permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    send_paste_shortcut()
}

fn clipboard_holds(text: &str, sequence: Option<NonZeroU32>) -> anyhow::Result<bool> {
    let _clipboard = open_clipboard()?;
    let mut current = Vec::new();
    clipboard_win::raw::get_string(&mut current)?;
    Ok(current == text.as_bytes() && sequence == clipboard_win::raw::seq_num())
}

fn send_paste_shortcut() -> anyhow::Result<Inserted> {
    let press = |key| synthetic_key(key, 0, 0);
    let release = |key| synthetic_key(key, 0, KEYEVENTF_KEYUP);
    let shortcut = [
        press(VK_CONTROL),
        press(VK_V),
        release(VK_V),
        release(VK_CONTROL),
    ];
    let sent = send_input(&shortcut)?;
    if sent == shortcut.len() {
        return Ok(Inserted::Sent);
    }
    if sent > 0 {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Release stuck synthetic modifiers after partial input even if the target also blocks cleanup"
        )]
        let _ = send_input(&[release(VK_V), release(VK_CONTROL)]);
    }
    Ok(Inserted::Copied(
        "Copied. This app blocked paste; press Ctrl+V.",
    ))
}

fn insert_direct(text: &str, permit: &InsertPermit) -> anyhow::Result<Inserted> {
    let input: Vec<INPUT> = text
        .replace("\r\n", "\n")
        .encode_utf16()
        .flat_map(|unit| {
            [
                synthetic_key(0, unit, KEYEVENTF_UNICODE),
                synthetic_key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ]
        })
        .collect();
    if let Some(outcome) = Delivery::Direct.preflight(has_external_target(), !modifiers_down()) {
        return Ok(outcome);
    }
    if !permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    let sent = send_input(&input)?;
    if sent != input.len() {
        return Ok(Inserted::Unavailable(
            "Direct input was blocked or only partly sent. Clipboard preserved.",
        ));
    }
    Ok(Inserted::Sent)
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
