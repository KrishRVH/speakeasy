use std::{
    num::NonZeroU32,
    thread,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::{INPUT, VK_CONTROL, VK_RETURN, VK_V},
    WindowsAndMessaging::GetForegroundWindow,
};
use zeroize::Zeroizing;

use super::{modifiers, press, release, send_input, type_unit, window_process};
use crate::{
    ClipboardContent, Inserted,
    clipboard::{self, Screen},
    insertion::NativeInsert,
};

const PRIVATE_FORMATS: [&str; 2] = ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"];

const CONCEALED: &str = "ExcludeClipboardContentFromMonitorProcessing";

const SYNTHESIZED: [u32; 2] = [
    clipboard_win::formats::CF_BITMAP,
    clipboard_win::formats::CF_PALETTE,
];

const HANDLES: [u32; 6] = [
    clipboard_win::formats::CF_METAFILEPICT,
    clipboard_win::formats::CF_ENHMETAFILE,
    clipboard_win::formats::CF_OWNERDISPLAY,
    clipboard_win::formats::CF_DSPBITMAP,
    clipboard_win::formats::CF_DSPMETAFILEPICT,
    clipboard_win::formats::CF_DSPENHMETAFILE,
];

const PRIVATE_AND_GDI: std::ops::RangeInclusive<u32> = 0x0200..=0x03FF;

const SAVED_BYTES: usize = 64 << 20;

type SavedFormat = (u32, Zeroizing<Vec<u8>>);

pub(crate) fn copy_text(text: &str) -> anyhow::Result<()> {
    write_clipboard(open_clipboard()?, text, None)
}

pub(crate) fn copy_content(
    content: &ClipboardContent,
    authorized: impl FnOnce() -> bool,
) -> anyhow::Result<bool> {
    clipboard::copy_when(authorized, open_clipboard, |lease| {
        write_clipboard(lease, &content.text, content.image.as_ref())
    })
}

fn saved_formats() -> Option<Vec<SavedFormat>> {
    let concealed = clipboard_win::raw::register_format(CONCEALED)?.get();
    let private =
        PRIVATE_FORMATS.map(|name| clipboard_win::raw::register_format(name).map(NonZeroU32::get));
    let mut size = 0_usize;
    clipboard::snapshot(
        clipboard_win::raw::EnumFormats::new(),
        |format| {
            if *format == concealed || HANDLES.contains(format) || PRIVATE_AND_GDI.contains(format)
            {
                Screen::Refuse
            } else if SYNTHESIZED.contains(format) || private.contains(&Some(*format)) {
                Screen::Skip
            } else {
                Screen::Save
            }
        },
        |format| {
            size = size
                .checked_add(clipboard_win::raw::size(format)?.get())
                .filter(|size| *size <= SAVED_BYTES)?;
            let mut data = Zeroizing::new(Vec::new());
            clipboard_win::raw::get_vec(format, &mut data).ok()?;
            Some((format, data))
        },
    )
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
    if let Some(saved) = saved {
        mark_private()?;
        for (format, data) in saved {
            clipboard_win::raw::set_without_clear(*format, data)?;
        }
    }
    Ok(())
}

fn mark_private() -> anyhow::Result<()> {
    for name in PRIVATE_FORMATS {
        let format = clipboard_win::raw::register_format(name)
            .context("Cannot register a clipboard format")?;
        clipboard_win::raw::set_without_clear(format.get(), &0_u32.to_ne_bytes())?;
    }
    Ok(())
}

fn write_clipboard(
    _lease: clipboard_win::Clipboard,
    text: &str,
    image: Option<&clipboard::Image>,
) -> anyhow::Result<()> {
    let image = image
        .map(|image| {
            clipboard_win::raw::register_format("PNG")
                .map(|png| (image, png))
                .context("Cannot register the clipboard image format")
        })
        .transpose()?;
    let write = || -> anyhow::Result<()> {
        clipboard_win::raw::empty()?;
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
    };
    write().map_err(|error| match clipboard_win::raw::empty() {
        Ok(()) => error,
        Err(_) => error
            .context("The clipboard may contain an incomplete copy; clear it before trying again"),
    })
}

pub(super) struct Insert {
    pub(super) shortcut: crate::WindowsShortcut,
    pub(super) typed: crate::insertion::KeyCount,
}

pub(super) enum Keys {
    Paste,
    Return,
    Text(Vec<INPUT>),
}

impl NativeInsert for Insert {
    type Activity = (u64, [i32; 2]);
    type Contents = Vec<SavedFormat>;
    type Keys = Keys;
    type Lease = clipboard_win::Clipboard;
    type Mark = Option<NonZeroU32>;
    type Target = usize;

    fn acquire(&mut self) -> anyhow::Result<clipboard_win::Clipboard> {
        open_clipboard()
    }

    fn save(&mut self, _lease: &clipboard_win::Clipboard) -> Option<Self::Contents> {
        saved_formats()
    }

    fn write(&mut self, lease: clipboard_win::Clipboard, text: &str) -> anyhow::Result<Self::Mark> {
        write_clipboard(lease, text, None)?;
        Ok(clipboard_win::raw::seq_num())
    }

    fn restore(
        &mut self,
        contents: Option<Self::Contents>,
        mark: &Option<NonZeroU32>,
    ) -> anyhow::Result<()> {
        restore_clipboard(contents.as_deref(), *mark)
    }

    fn mark(&mut self) -> Option<NonZeroU32> {
        clipboard_win::raw::seq_num()
    }

    fn holds(&mut self, text: &str, mark: &Option<NonZeroU32>) -> anyhow::Result<bool> {
        let _clipboard = open_clipboard()?;

        if mark.is_none()
            || *mark != clipboard_win::raw::seq_num()
            || !clipboard_win::raw::is_format_avail(clipboard_win::formats::CF_UNICODETEXT)
        {
            return Ok(false);
        }
        let mut current = Zeroizing::new(Vec::new());
        clipboard_win::raw::get_string(&mut current)?;
        Ok(current.as_slice() == windows_line_endings(text).as_bytes())
    }

    fn paste_keys(&mut self) -> anyhow::Result<Keys> {
        Ok(Keys::Paste)
    }

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Keys> {
        Ok(Keys::Text(
            text.replace("\r\n", "\n")
                .encode_utf16()
                .flat_map(|unit| match unit {
                    10 | 13 => [press(VK_RETURN), release(VK_RETURN)],
                    _ => type_unit(unit),
                })
                .collect(),
        ))
    }

    fn return_keys(&mut self) -> anyhow::Result<Keys> {
        Ok(Keys::Return)
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
        !modifiers(self.shortcut).any()
    }

    fn send(&mut self, keys: Keys) -> anyhow::Result<Inserted> {
        match keys {
            Keys::Paste => {
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
                    "Text copied. This app blocked paste; press Ctrl+V.",
                ))
            },
            Keys::Return => {
                let keys = [press(VK_RETURN), release(VK_RETURN)];
                let sent = send_input(&keys)?;
                if sent == keys.len() {
                    return Ok(Inserted::Sent);
                }
                if sent > 0 {
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "Release a stuck synthetic Return after partial input even if the target also blocks cleanup"
                    )]
                    let _ = send_input(&[release(VK_RETURN)]);
                }
                Ok(Inserted::Unavailable("This app blocked Return."))
            },
            Keys::Text(input) => {
                let sent = send_input(&input)?;
                if sent != input.len() {
                    if sent % 2 == 1
                        && input
                            .get(sent.saturating_sub(1))
                            .is_some_and(|key| unsafe { key.Anonymous.ki.wVk == VK_RETURN })
                    {
                        #[expect(
                            clippy::let_underscore_must_use,
                            reason = "Release a partial synthetic line break even if the target blocks cleanup"
                        )]
                        let _ = send_input(&[release(VK_RETURN)]);
                    }
                    return Ok(Inserted::Unavailable(
                        "Typing was blocked or cut short. Turn off “Type instead of paste” in Settings.",
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
