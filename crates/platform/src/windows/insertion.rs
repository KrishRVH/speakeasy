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
use crate::{ClipboardContent, Inserted, clipboard, insertion::NativeInsert};

/// Registered clipboard formats whose zero `DWORD` keeps Speakeasy's clipboard text out of Windows
/// clipboard history and cloud sync. Apps still paste it, and it stays on the clipboard.
const PRIVATE_FORMATS: [&str; 2] = ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"];

pub(crate) fn copy_text(text: &str) -> anyhow::Result<()> {
    let mut insert = Insert;
    let lease = insert.acquire()?;
    insert.write(lease, text)
}

pub(crate) fn copy_content(
    content: &ClipboardContent,
    authorized: impl FnOnce() -> bool,
) -> anyhow::Result<bool> {
    clipboard::copy_when(authorized, open_clipboard, |lease| {
        write_clipboard(lease, &content.text, content.image.as_ref())
    })
}

fn write_clipboard(
    lease: clipboard_win::Clipboard,
    text: &str,
    image: Option<&clipboard::Image>,
) -> anyhow::Result<()> {
    let png = image
        .map(|_| {
            clipboard_win::raw::register_format("PNG")
                .context("Cannot register the clipboard image format")
        })
        .transpose()?;
    let write = || -> anyhow::Result<()> {
        clipboard_win::raw::empty()?;
        for name in PRIVATE_FORMATS {
            let format = clipboard_win::raw::register_format(name)
                .context("Cannot register a clipboard format")?;
            clipboard_win::raw::set_without_clear(format.get(), &0_u32.to_ne_bytes())?;
        }
        if let Some((image, png)) = image.zip(png) {
            clipboard_win::raw::set_without_clear(png.get(), &image.png)?;
            clipboard_win::raw::set_without_clear(clipboard_win::formats::CF_DIBV5, &image.dib)?;
        }
        clipboard_win::raw::set_string_with(
            &windows_line_endings(text),
            clipboard_win::options::NoClear,
        )?;
        Ok(())
    };
    let result = match write() {
        Ok(()) => Ok(()),
        Err(error) => match clipboard_win::raw::empty() {
            Ok(()) => Err(error),
            Err(_) => Err(error.context(
                "The clipboard may contain an incomplete copy; clear it before trying again",
            )),
        },
    };
    drop(lease);
    result
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
        write_clipboard(lease, text, None)
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

    /// Set only in the child that [`copied_text_stays_private_and_a_copied_image_is_a_change`]
    /// starts.
    const PRIVATE_STATION: &str = "SPEAKEASY_PRIVATE_WINDOW_STATION";
    const TEXT: &str = "fixture\ntext";

    #[test]
    #[ignore = "Starts a child on a private window station, whose clipboard is never the desktop's"]
    fn copied_text_stays_private_and_a_copied_image_is_a_change() -> anyhow::Result<()> {
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
        // The helper's last write is the image, so the desktop's clipboard would hold it had the
        // helper written there.
        let _clipboard = open_clipboard()?;
        ensure!(
            !clipboard_win::raw::is_format_avail(image_format()?.get()),
            "The helper copied to the desktop's clipboard"
        );
        Ok(())
    }

    #[test]
    #[ignore = "Subprocess helper; does nothing unless copied_text_stays_private_and_a_copied_image_is_a_change starts it"]
    fn check_on_a_private_window_station() -> anyhow::Result<()> {
        if std::env::var_os(PRIVATE_STATION).is_none() {
            return Ok(());
        }
        enter_private_station()?;
        copy_text(TEXT)?;
        assert_private_text("the copied text")?;
        let mut insert = Insert;
        let lease = insert.acquire()?;
        insert.write(lease, TEXT)?;
        let mark = insert.mark();
        assert!(
            insert.holds(TEXT, &mark)?,
            "The clipboard lost the text just written"
        );
        assert_private_text("the pasted text")?;
        assert_private_image()?;
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

    fn assert_private_image() -> anyhow::Result<()> {
        let content = ClipboardContent::moment(
            TEXT.into(),
            2,
            2,
            vec![3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255],
        )?;
        let before = clipboard_win::raw::seq_num();
        assert!(!copy_content(&content, || false)?);
        assert_eq!(
            before,
            clipboard_win::raw::seq_num(),
            "A cancelled copy changed the clipboard"
        );
        assert!(copy_content(&content, || true)?);
        assert_private_text("the image's context")?;
        let image = content
            .image
            .as_ref()
            .context("The fixture contains an image")?;
        let _clipboard = open_clipboard()?;
        let png_format = clipboard_win::raw::register_format("PNG").context("PNG format")?;
        for (format, expected) in [
            (png_format.get(), &image.png),
            (clipboard_win::formats::CF_DIBV5, &image.dib),
        ] {
            let mut bytes = Vec::new();
            clipboard_win::raw::get_vec(format, &mut bytes)?;
            assert_eq!(
                &bytes, expected,
                "The native clipboard lost a visual representation"
            );
        }
        // Ask Windows to synthesize the legacy bitmap format, as a native image consumer does.
        // This checks that our DIB header is usable, beyond reading our own bytes back unchanged.
        let mut bitmap = Vec::new();
        clipboard_win::raw::get_bitmap(&mut bitmap)?;
        let word = |at: usize| {
            u32::from_le_bytes(bitmap[at..at.checked_add(4).unwrap()].try_into().unwrap())
        };
        assert_eq!((word(18), word(22)), (2, 2));
        assert_eq!(u16::from_le_bytes(bitmap[28..30].try_into().unwrap()), 32);
        let offset = usize::try_from(word(10))?;
        let colors: Vec<_> = bitmap[offset..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| &pixel[..3])
            .collect();
        assert_eq!(
            colors,
            [&[9, 8, 7][..], &[12, 11, 10], &[3, 2, 1], &[6, 5, 4]],
            "Native bitmap conversion changed colors or orientation"
        );
        Ok(())
    }

    /// Asserts that the clipboard holds [`TEXT`] with Windows line endings, kept out of history and
    /// cloud sync; `source` names the write that put it there.
    fn assert_private_text(source: &str) -> anyhow::Result<()> {
        let _clipboard = open_clipboard()?;
        let mut text = Vec::new();
        clipboard_win::raw::get_string(&mut text)?;
        assert_eq!(
            text, b"fixture\r\ntext",
            "The clipboard lost {source} or its Windows line endings"
        );
        for name in PRIVATE_FORMATS {
            let format = clipboard_win::raw::register_format(name)
                .context("Cannot register a clipboard format")?;
            let mut value = [u8::MAX; 4];
            clipboard_win::raw::get(format.get(), &mut value)
                .with_context(|| format!("{name} is missing from {source}"))?;
            assert_eq!(value, [0; 4], "{name} lets {source} into history");
        }
        Ok(())
    }

    /// Moves this helper process onto a window station and desktop apart from the user's. Its
    /// clipboard belongs to that station, which interactive apps never use; the process keeps both
    /// until it exits.
    fn enter_private_station() -> anyhow::Result<()> {
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
        let name = to_wide("speakeasy-test");
        // SAFETY: the NUL-terminated name outlives the call; null device, mode, and attributes take
        // the defaults. The desktop opens in the station just entered.
        let desktop =
            unsafe { CreateDesktopW(name.as_ptr(), null(), null(), 0, GENERIC_ALL, null()) };
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
        let format = image_format()?;
        let _clipboard = open_clipboard()?;
        clipboard_win::raw::set(format.get(), &[1, 2, 3])?;
        Ok(())
    }

    fn image_format() -> anyhow::Result<NonZeroU32> {
        clipboard_win::raw::register_format("Speakeasy test image")
            .context("Cannot register a clipboard format")
    }
}
