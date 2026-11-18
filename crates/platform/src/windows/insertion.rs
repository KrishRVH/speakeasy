use std::{
    num::{NonZeroU32, NonZeroUsize},
    thread,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::{INPUT, VK_CONTROL, VK_RETURN, VK_V},
    WindowsAndMessaging::GetForegroundWindow,
};

use super::{modifiers, press, release, send_input, type_unit, window_process};
use crate::{ClipboardContent, Inserted, clipboard, insertion::NativeInsert};

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

fn saved_formats() -> Option<Vec<(u32, Vec<u8>)>> {
    let concealed = clipboard_win::raw::register_format(CONCEALED)?.get();
    let private =
        PRIVATE_FORMATS.map(|name| clipboard_win::raw::register_format(name).map(NonZeroU32::get));
    let mut size = 0_usize;
    let mut saved = Vec::new();
    for format in clipboard_win::raw::EnumFormats::new() {
        if format == concealed || HANDLES.contains(&format) || PRIVATE_AND_GDI.contains(&format) {
            return None;
        }
        if SYNTHESIZED.contains(&format) || private.contains(&Some(format)) {
            continue;
        }
        size = size
            .checked_add(clipboard_win::raw::size(format).map_or(0, NonZeroUsize::get))
            .filter(|size| *size <= SAVED_BYTES)?;
        let mut data = Vec::new();
        clipboard_win::raw::get_vec(format, &mut data).ok()?;
        saved.push((format, data));
    }
    (!saved.is_empty()).then_some(saved)
}

fn restore_clipboard(saved: &[(u32, Vec<u8>)], mark: Option<NonZeroU32>) -> anyhow::Result<()> {
    let _clipboard = open_clipboard()?;
    if clipboard_win::raw::seq_num() != mark {
        return Ok(());
    }
    clipboard_win::raw::empty()?;
    mark_private()?;
    for (format, data) in saved {
        clipboard_win::raw::set_without_clear(*format, data)?;
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
    type Contents = Vec<(u32, Vec<u8>)>;
    type Keys = Keys;
    type Lease = clipboard_win::Clipboard;
    type Mark = Option<NonZeroU32>;
    type Target = usize;

    fn acquire(&mut self) -> anyhow::Result<clipboard_win::Clipboard> {
        open_clipboard()
    }

    fn save(&mut self, _lease: &clipboard_win::Clipboard) -> Option<Vec<(u32, Vec<u8>)>> {
        saved_formats()
    }

    fn write(&mut self, lease: clipboard_win::Clipboard, text: &str) -> anyhow::Result<()> {
        write_clipboard(lease, text, None)
    }

    fn restore(
        &mut self,
        contents: Vec<(u32, Vec<u8>)>,
        mark: &Option<NonZeroU32>,
    ) -> anyhow::Result<()> {
        restore_clipboard(&contents, *mark)
    }

    fn mark(&mut self) -> Option<NonZeroU32> {
        clipboard_win::raw::seq_num()
    }

    fn holds(&mut self, text: &str, mark: &Option<NonZeroU32>) -> anyhow::Result<bool> {
        let _clipboard = open_clipboard()?;

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
        Ok(Keys::Paste)
    }

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Keys> {
        Ok(Keys::Text(
            text.replace("\r\n", "\n")
                .encode_utf16()
                .flat_map(type_unit)
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
        let mut insert = Insert {
            shortcut: crate::WindowsShortcut::default(),
            typed: crate::insertion::KeyCount::default(),
        };
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

        let mark = insert.mark();
        assert!(
            !insert.holds(TEXT, &mark)?,
            "A copy with no text must read as a change even under its own mark"
        );
        assert_restores_only_what_it_replaced(&mut insert)
    }

    fn assert_restores_only_what_it_replaced(insert: &mut Insert) -> anyhow::Result<()> {
        let html = clipboard_win::raw::register_format("HTML Format")
            .context("Cannot register a clipboard format")?;
        copy_previous(html, false)?;
        let lease = insert.acquire()?;
        let saved = insert
            .save(&lease)
            .context("Ordinary text and HTML were not saved")?;
        insert.write(lease, TEXT)?;
        let mark = insert.mark();
        insert.restore(saved, &mark)?;
        {
            let _clipboard = open_clipboard()?;
            let mut text = Vec::new();
            clipboard_win::raw::get_string(&mut text)?;
            assert_eq!(text, b"previous", "The replaced text did not return");
            let mut markup = Vec::new();
            clipboard_win::raw::get_vec(html.get(), &mut markup)?;
            assert_eq!(
                markup, b"<b>previous</b>",
                "The replaced HTML did not return"
            );
        }

        let lease = insert.acquire()?;
        let saved = insert
            .save(&lease)
            .context("The previous copy was not saved")?;
        insert.write(lease, TEXT)?;
        let mark = insert.mark();
        copy_without_text()?;
        insert.restore(saved, &mark)?;
        {
            let _clipboard = open_clipboard()?;
            assert!(
                clipboard_win::raw::is_format_avail(image_format()?.get()),
                "A restore replaced a newer copy"
            );
        }

        copy_previous(html, true)?;
        let lease = insert.acquire()?;
        assert!(
            insert.save(&lease).is_none(),
            "A concealed copy was kept to restore"
        );
        Ok(())
    }

    fn copy_previous(html: NonZeroU32, concealed: bool) -> anyhow::Result<()> {
        let _clipboard = open_clipboard()?;
        clipboard_win::raw::empty()?;
        if concealed {
            let format = clipboard_win::raw::register_format(CONCEALED)
                .context("Cannot register a clipboard format")?;
            clipboard_win::raw::set_without_clear(format.get(), &[0])?;
        }
        clipboard_win::raw::set_without_clear(html.get(), b"<b>previous</b>")?;
        clipboard_win::raw::set_string_with("previous", clipboard_win::options::NoClear)?;
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

    fn enter_private_station() -> anyhow::Result<()> {
        let station = unsafe { CreateWindowStationW(null(), 0, GENERIC_ALL, null()) };
        ensure!(
            !station.is_null(),
            "Cannot create a window station: {}",
            std::io::Error::last_os_error()
        );

        ok_or_last_error(unsafe { SetProcessWindowStation(station) })?;
        let name = to_wide("speakeasy-test");

        let desktop =
            unsafe { CreateDesktopW(name.as_ptr(), null(), null(), 0, GENERIC_ALL, null()) };
        ensure!(
            !desktop.is_null(),
            "Cannot create a desktop: {}",
            std::io::Error::last_os_error()
        );

        ok_or_last_error(unsafe { SetThreadDesktop(desktop) })?;
        Ok(())
    }

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
