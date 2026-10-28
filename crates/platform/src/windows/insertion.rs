//! Clipboard paste and direct text input on Windows.

use std::{
    num::NonZeroU32,
    thread,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::{INPUT, VK_CONTROL, VK_V},
    WindowsAndMessaging::GetForegroundWindow,
};

use super::{modifiers, press, release, send_input, type_unit, window_process};
use crate::{Inserted, insertion::NativeInsert};

/// Registered clipboard formats whose zero `DWORD` keeps a paste's text out of Windows clipboard
/// history and cloud sync. Apps still paste it, and it stays on the clipboard.
const PRIVATE_FORMATS: [&str; 2] = ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"];

pub(crate) fn copy_text(text: &str) -> anyhow::Result<()> {
    let text = windows_line_endings(text);
    let _clipboard = open_clipboard()?;
    clipboard_win::raw::set_string(&text)?;
    Ok(())
}

/// The native steps of one Windows insertion, as [`NativeInsert`] defines them.
pub(crate) struct Insert;

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

    /// Marks the clipboard private before the text arrives, so no failure leaves the text
    /// unmarked.
    fn write(&mut self, lease: clipboard_win::Clipboard, text: &str) -> anyhow::Result<()> {
        clipboard_win::raw::empty()?;
        for name in PRIVATE_FORMATS {
            let format = clipboard_win::raw::register_format(name)
                .context("Cannot register a clipboard format")?;
            clipboard_win::raw::set_without_clear(format.get(), &0_u32.to_ne_bytes())?;
        }
        clipboard_win::raw::set_string_with(
            &windows_line_endings(text),
            clipboard_win::options::NoClear,
        )?;
        drop(lease);
        Ok(())
    }

    fn mark(&mut self) -> Option<NonZeroU32> {
        clipboard_win::raw::seq_num()
    }

    fn holds(&mut self, text: &str, mark: &Option<NonZeroU32>) -> anyhow::Result<bool> {
        let _clipboard = open_clipboard()?;
        // An image or files another app copied hold no text to read; that is a change, not a fault.
        if *mark != clipboard_win::raw::seq_num()
            || !clipboard_win::raw::is_format_avail(clipboard_win::formats::CF_UNICODETEXT)
        {
            return Ok(false);
        }
        let mut current = Vec::new();
        clipboard_win::raw::get_string(&mut current)?;
        Ok(current == windows_line_endings(text).as_bytes())
    }

    fn paste_keys(&mut self) -> anyhow::Result<Keys> {
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
                .flat_map(type_unit)
                .collect(),
        ))
    }

    fn external_focus(&mut self) -> bool {
        // SAFETY: takes no arguments; a null answer names no window, which has no process.
        let focused = unsafe { GetForegroundWindow() };
        window_process(focused).is_some_and(|process| process != std::process::id())
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
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "Release stuck synthetic modifiers after partial input even if the target also blocks cleanup"
                    )]
                    let _ = send_input(&[release(VK_V), release(VK_CONTROL)]);
                }
                Ok(Inserted::Copied(
                    "Text copied. This app blocked paste; press Ctrl+V.",
                ))
            },
            Keys::Text(input) => {
                let sent = send_input(&input)?;
                if sent != input.len() {
                    return Ok(Inserted::Unavailable(
                        "Typing was blocked or cut short. Dictate again, or turn off Keep clipboard in Settings to paste.",
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
/// it right after a change. The retries block, so callers stay off the UI thread.
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

#[cfg(test)]
mod tests {
    use std::{process::Command, ptr::null};

    use anyhow::ensure;
    use windows_sys::Win32::{
        Foundation::GENERIC_ALL,
        System::StationsAndDesktops::{
            CreateDesktopW, CreateWindowStationW, SetProcessWindowStation, SetThreadDesktop,
        },
    };

    use super::*;
    use crate::windows::{ok_or_last_error, to_wide};

    /// Set only in the child that [`a_copied_image_is_a_clipboard_change_not_a_failure`] starts.
    const PRIVATE_STATION: &str = "SPEAKEASY_PRIVATE_WINDOW_STATION";
    const TEXT: &str = "fixture\ntext";

    #[test]
    #[ignore = "Starts a child on a private window station, whose clipboard is never the desktop's"]
    fn a_copied_image_is_a_clipboard_change_not_a_failure() -> anyhow::Result<()> {
        let module = module_path!()
            .split_once("::")
            .map_or(module_path!(), |(_, module)| module);
        let child = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                &format!("{module}::check_on_a_private_window_station"),
                "--ignored",
            ])
            .env(PRIVATE_STATION, "1")
            .output()?;
        let report = String::from_utf8_lossy(&child.stdout);
        ensure!(
            child.status.success() && report.contains("test result: ok. 1 passed"),
            "The private window station check failed:\n{report}"
        );
        let _clipboard = open_clipboard()?;
        ensure!(
            !clipboard_win::raw::is_format_avail(private_format()?.get()),
            "The helper copied to the desktop's clipboard"
        );
        Ok(())
    }

    #[test]
    #[ignore = "Subprocess helper; does nothing unless a_copied_image_is_a_clipboard_change_not_a_failure starts it"]
    fn check_on_a_private_window_station() -> anyhow::Result<()> {
        if std::env::var_os(PRIVATE_STATION).is_none() {
            return Ok(());
        }
        enter_private_station()?;
        let mut insert = Insert;
        let lease = insert.acquire()?;
        insert.write(lease, TEXT)?;
        let mark = insert.mark();
        assert!(
            insert.holds(TEXT, &mark)?,
            "The clipboard lost the text just written"
        );
        {
            let _clipboard = open_clipboard()?;
            for name in PRIVATE_FORMATS {
                let format = clipboard_win::raw::register_format(name)
                    .context("Cannot register a clipboard format")?;
                let mut value = [u8::MAX; 4];
                clipboard_win::raw::get(format.get(), &mut value)?;
                assert_eq!(value, [0; 4], "{name} lets the pasted text into history");
            }
        }
        copy_without_text()?;
        assert!(
            !insert.holds(TEXT, &mark)?,
            "A copy with no text must read as a change"
        );
        // The copy may also land between the write and the mark.
        let mark = insert.mark();
        assert!(
            !insert.holds(TEXT, &mark)?,
            "A copy with no text must read as a change even under its own mark"
        );
        Ok(())
    }

    /// Moves this helper process onto a window station and desktop apart from the user's. Its
    /// clipboard belongs to that station, which interactive apps never use; the process keeps both
    /// until it exits.
    fn enter_private_station() -> anyhow::Result<()> {
        let desktop = to_wide("speakeasy-test");
        // SAFETY: a null name, which only administrators may replace, asks for the station named
        // after this logon session; null attributes take the default security.
        let station = unsafe { CreateWindowStationW(null(), 0, GENERIC_ALL, null()) };
        ensure!(
            !station.is_null(),
            "Cannot create a window station: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the station handle is live; only this helper process changes stations.
        ok_or_last_error(unsafe { SetProcessWindowStation(station) })?;
        // SAFETY: the NUL-terminated name outlives the call; null device, mode, and attributes take
        // the defaults. The desktop opens in the station just entered.
        let desktop =
            unsafe { CreateDesktopW(desktop.as_ptr(), null(), null(), 0, GENERIC_ALL, null()) };
        ensure!(
            !desktop.is_null(),
            "Cannot create a desktop: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the desktop handle is live, and this test thread has made no window calls yet,
        // which a desktop change requires.
        ok_or_last_error(unsafe { SetThreadDesktop(desktop) })?;
        Ok(())
    }

    /// Copies data in a format of its own, as an app copying an image or files would.
    fn copy_without_text() -> anyhow::Result<()> {
        let format = private_format()?;
        let _clipboard = open_clipboard()?;
        clipboard_win::raw::set(format.get(), &[1, 2, 3])?;
        Ok(())
    }

    fn private_format() -> anyhow::Result<NonZeroU32> {
        clipboard_win::raw::register_format("Speakeasy test image")
            .context("Cannot register a clipboard format")
    }
}
