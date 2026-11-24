use std::{
    num::NonZeroU32,
    thread,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::{INPUT, VK_CONTROL, VK_RETURN, VK_SHIFT, VK_V},
    WindowsAndMessaging::GetForegroundWindow,
};

use super::{is_key_down, press, release, send_input, type_unit, window_process};
use crate::{
    ClipboardContent, Inserted, clipboard,
    insertion::{self, NativeInsert, Typed},
};

/// Formats whose zero DWORD keeps a copy out of clipboard history and cloud sync.
const PRIVATE_FORMATS: [&str; 2] = ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"];

/// Formats Windows synthesizes from a DIB, so restoring the DIB brings them back.
const SYNTHESIZED: [u32; 2] = [
    clipboard_win::formats::CF_BITMAP,
    clipboard_win::formats::CF_PALETTE,
];

/// Formats whose data are handles, not global-memory bytes a snapshot can copy.
const HANDLES: [u32; 6] = [
    clipboard_win::formats::CF_METAFILEPICT,
    clipboard_win::formats::CF_ENHMETAFILE,
    clipboard_win::formats::CF_OWNERDISPLAY,
    clipboard_win::formats::CF_DSPBITMAP,
    clipboard_win::formats::CF_DSPMETAFILEPICT,
    clipboard_win::formats::CF_DSPENHMETAFILE,
];

/// `CF_PRIVATEFIRST` through `CF_GDIOBJLAST`, whose data are handles too.
const PRIVATE_AND_GDI: std::ops::RangeInclusive<u32> = 0x0200..=0x03FF;

type SavedFormat = (u32, Vec<u8>);

pub(crate) fn copy_text(text: &str) -> anyhow::Result<()> {
    write_clipboard(open_clipboard()?, || fill(text, None))
}

pub(crate) fn copy_content(
    content: &ClipboardContent,
    authorized: impl FnOnce() -> bool,
) -> anyhow::Result<bool> {
    clipboard::copy_when(authorized, open_clipboard, |lease| {
        let image = content
            .image
            .as_ref()
            .map(|image| {
                clipboard_win::raw::register_format("PNG")
                    .map(|png| (image, png))
                    .context("Can't register the clipboard image format")
            })
            .transpose()?;
        write_clipboard(lease, || fill(&content.text, image))
    })
}

fn saved_formats() -> Option<Vec<SavedFormat>> {
    let private = private_formats();
    let mut kept = Vec::new();
    for format in clipboard_win::raw::EnumFormats::new() {
        // One uncopyable format makes a faithful restore impossible, so keep no snapshot.
        if HANDLES.contains(&format) || PRIVATE_AND_GDI.contains(&format) {
            return None;
        }
        if !SYNTHESIZED.contains(&format) && !private.contains(&Some(format)) {
            kept.push(format);
        }
    }
    let saved = kept
        .into_iter()
        .map(|format| {
            let mut data = Vec::new();
            clipboard_win::raw::get_vec(format, &mut data).ok()?;
            Some((format, data))
        })
        .collect::<Option<Vec<_>>>()?;
    (!saved.is_empty()).then_some(saved)
}

fn restore_clipboard(
    saved: Option<&[SavedFormat]>,
    mark: Option<NonZeroU32>,
) -> anyhow::Result<()> {
    let _clipboard = open_clipboard()?;
    if mark.is_none() || clipboard_win::raw::seq_num() != mark {
        return Ok(());
    }
    clipboard_win::raw::empty()?;
    saved.map_or(Ok(()), put_back)
}

fn put_back(saved: &[SavedFormat]) -> anyhow::Result<()> {
    mark_private()?;
    for (format, data) in saved {
        clipboard_win::raw::set_without_clear(*format, data)?;
    }
    Ok(())
}

fn mark_private() -> anyhow::Result<()> {
    for name in PRIVATE_FORMATS {
        let format = clipboard_win::raw::register_format(name)
            .context("Can't register a clipboard format")?;
        clipboard_win::raw::set_without_clear(format.get(), &0_u32.to_ne_bytes())?;
    }
    Ok(())
}

fn write_clipboard(
    _lease: clipboard_win::Clipboard,
    fill: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    clipboard_win::raw::empty()?;
    fill()
}

fn fill(text: &str, image: Option<(&clipboard::Image, NonZeroU32)>) -> anyhow::Result<()> {
    // Mark first so no failure leaves the copied text unmarked.
    mark_private()?;
    if let Some((image, png)) = image {
        clipboard_win::raw::set_without_clear(png.get(), &image.png)?;
        clipboard_win::raw::set_without_clear(clipboard_win::formats::CF_DIBV5, &image.dib)?;
    }
    clipboard_win::raw::set_string_with(
        &windows_line_endings(text),
        clipboard_win::options::NoClear,
    )?;
    Ok(())
}

pub(super) struct Insert {
    pub(super) shortcut: crate::WindowsShortcut,
    pub(super) typed: insertion::KeyCount,
}

pub(super) struct Keys {
    input: Vec<INPUT>,
    blocked: Inserted,
}

impl NativeInsert for Insert {
    type Activity = (u64, [i32; 2]);
    type Contents = Vec<SavedFormat>;
    type Keys = Keys;
    type Mark = Option<NonZeroU32>;
    type Target = usize;

    fn copy(&mut self, text: &str) -> anyhow::Result<(Self::Mark, Option<Self::Contents>)> {
        let lease = open_clipboard()?;
        let saved = saved_formats();
        write_clipboard(lease, || fill(text, None))?;
        Ok((clipboard_win::raw::seq_num(), saved))
    }

    fn restore(
        &mut self,
        contents: Option<Self::Contents>,
        mark: &Option<NonZeroU32>,
    ) -> anyhow::Result<()> {
        restore_clipboard(contents.as_deref(), *mark)
    }

    fn paste_keys(&mut self) -> anyhow::Result<Keys> {
        Ok(Keys {
            input: vec![
                press(VK_CONTROL),
                press(VK_V),
                release(VK_V),
                release(VK_CONTROL),
            ],
            blocked: Inserted::Copied("Text copied. This app blocked paste; press Ctrl+V."),
        })
    }

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Keys> {
        let mut input = Vec::new();
        for typed in insertion::split_line_breaks(text) {
            match typed {
                Typed::Text(run) => input.extend(run.encode_utf16().flat_map(type_unit)),
                Typed::LineBreak => input.extend([
                    press(VK_SHIFT),
                    press(VK_RETURN),
                    release(VK_RETURN),
                    release(VK_SHIFT),
                ]),
            }
        }
        Ok(Keys {
            input,
            blocked: Inserted::Unavailable(
                "Typing was blocked or cut short. Turn off “Type instead of paste” in Settings.",
            ),
        })
    }

    fn return_keys(&mut self) -> anyhow::Result<Keys> {
        Ok(Keys {
            input: vec![press(VK_RETURN), release(VK_RETURN)],
            blocked: Inserted::Unavailable("This app blocked Return."),
        })
    }

    fn external_focus(&mut self) -> bool {
        let focused = unsafe { GetForegroundWindow() };
        window_process(focused).is_some_and(|process| process != std::process::id())
    }

    fn target(&mut self) -> Option<usize> {
        let focused = unsafe { GetForegroundWindow() };
        (!focused.is_null()).then(|| focused.addr())
    }

    fn activity(&mut self) -> Option<Self::Activity> {
        let pointer = super::screen::pointer_location()?;
        Some((self.typed.read(), [pointer.x, pointer.y]))
    }

    fn shortcut_released(&mut self) -> bool {
        !self.shortcut.modifiers(is_key_down).any()
    }

    fn send(&mut self, keys: Keys) -> anyhow::Result<Inserted> {
        let sent = send_input(&keys.input)?;
        Ok(if sent == keys.input.len() {
            Inserted::Sent
        } else {
            keys.blocked
        })
    }
}

fn private_formats() -> [Option<u32>; 2] {
    PRIVATE_FORMATS.map(|name| clipboard_win::raw::register_format(name).map(NonZeroU32::get))
}

fn windows_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Opens the clipboard, retrying briefly since listeners can hold it right after a change.
///
/// The retries block, so callers stay off the UI thread.
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
mod tests;
