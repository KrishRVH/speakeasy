use std::{process::Command, ptr::null};

use anyhow::ensure;
use windows_sys::Win32::{
    Foundation::GENERIC_ALL,
    System::StationsAndDesktops::{
        CreateDesktopW, CreateWindowStationW, SetProcessWindowStation, SetThreadDesktop,
    },
    UI::Input::KeyboardAndMouse::KEYEVENTF_UNICODE,
};

use super::*;
use crate::windows::{OWN_INPUT, ok_or_last_error, to_wide};

const PRIVATE_STATION: &str = "SPEAKEASY_PRIVATE_WINDOW_STATION";
const TEXT: &str = "fixture\ntext";

const UNICODE: u32 = KEYEVENTF_UNICODE;
const UP: u32 = KEYEVENTF_KEYUP;

fn strokes(input: &[INPUT]) -> Vec<(u16, u16, u32)> {
    input
        .iter()
        .map(|input| {
            let key = unsafe { input.Anonymous.ki };
            assert_eq!(
                key.dwExtraInfo, OWN_INPUT,
                "A synthetic key lacks the own-input marker"
            );
            (key.wVk, key.wScan, key.dwFlags)
        })
        .collect()
}

fn insert() -> Insert {
    Insert {
        shortcut: crate::WindowsShortcut::default(),
        typed: crate::insertion::KeyCount::default(),
    }
}

#[test]
fn typed_line_breaks_are_shift_enter_and_text_keeps_utf16() -> anyhow::Result<()> {
    let line_break = [
        (VK_SHIFT, 0, 0),
        (VK_RETURN, 0, 0),
        (VK_RETURN, 0, UP),
        (VK_SHIFT, 0, UP),
    ];
    let typed = |unit| [(0, unit, UNICODE), (0, unit, UNICODE | UP)];
    let expected: Vec<_> = [
        typed(97).as_slice(),
        &line_break,
        &typed(0xD83E),
        &typed(0xDD80),
        &line_break,
        &typed(98),
        &line_break,
    ]
    .concat();
    assert_eq!(
        strokes(&insert().text_keys("a\r\n🦀\rb\n")?.input),
        expected
    );
    Ok(())
}

#[test]
fn input_cut_short_releases_every_key_it_left_down() -> anyhow::Result<()> {
    let keys = insert().text_keys("a\nb")?;
    let shift_up = (VK_SHIFT, 0, UP);
    for (sent, released) in [
        (0, vec![]),
        (1, vec![(0, 97, UNICODE | UP)]),
        (2, vec![]),
        (3, vec![shift_up]),
        (4, vec![(VK_RETURN, 0, UP), shift_up]),
        (5, vec![shift_up]),
        (6, vec![]),
        (7, vec![(0, 98, UNICODE | UP)]),
    ] {
        assert_eq!(
            strokes(&still_down(&keys.input[..sent])),
            released,
            "Input cut short after {sent} keys"
        );
    }
    let paste = insert().paste_keys()?;
    assert_eq!(
        strokes(&still_down(&paste.input[..2])),
        [(0x56, 0, UP), (VK_CONTROL, 0, UP)]
    );
    Ok(())
}

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
    let written = insert.write(lease, TEXT, None)?;
    assert_eq!(
        written,
        insert.mark(),
        "The write's mark is not the clipboard's sequence number after the write"
    );
    assert!(
        insert.holds(TEXT, &written)?,
        "The clipboard lost the text just written"
    );
    assert_private_text("the pasted text")?;
    assert_private_image()?;
    copy_without_text()?;
    assert!(
        !insert.holds(TEXT, &written)?,
        "A copy with no text must read as a change"
    );

    let mark = insert.mark();
    assert!(
        !insert.holds(TEXT, &mark)?,
        "A copy with no text must read as a change even under its own mark"
    );
    assert_an_interrupted_listing_is_refused()?;
    assert_a_failed_write_puts_back_what_it_replaced(&mut insert)?;
    assert_restores_only_what_it_replaced(&mut insert)
}

fn assert_a_failed_write_puts_back_what_it_replaced(insert: &mut Insert) -> anyhow::Result<()> {
    let html = clipboard_win::raw::register_format("HTML Format")
        .context("Cannot register a clipboard format")?;
    copy_previous(html, false)?;
    let lease = insert.acquire()?;
    let saved = insert
        .save(&lease)
        .context("Ordinary text and HTML were not saved")?;
    clipboard_win::raw::empty()?;
    clipboard_win::raw::set_string_with("partial", clipboard_win::options::NoClear)?;
    roll_back(Some(&saved))?;
    drop(lease);
    assert_previous(html, "A failed write")?;

    let lease = insert.acquire()?;
    clipboard_win::raw::set_string_with("partial", clipboard_win::options::NoClear)?;
    roll_back(None)?;
    drop(lease);
    let _clipboard = open_clipboard()?;
    assert_eq!(
        clipboard_win::raw::count_formats(),
        Some(0),
        "A failed write with nothing to put back left part of its copy"
    );
    Ok(())
}

fn assert_previous(html: NonZeroU32, source: &str) -> anyhow::Result<()> {
    let _clipboard = open_clipboard()?;
    let mut text = Vec::new();
    clipboard_win::raw::get_string(&mut text)
        .with_context(|| format!("{source} lost the replaced text"))?;
    assert_eq!(text, b"previous", "{source} lost the replaced text");
    let mut markup = Vec::new();
    clipboard_win::raw::get_vec(html.get(), &mut markup)?;
    assert_eq!(
        markup, b"<b>previous</b>",
        "{source} lost the replaced HTML"
    );
    Ok(())
}

fn assert_an_interrupted_listing_is_refused() -> anyhow::Result<()> {
    let html = clipboard_win::raw::register_format("HTML Format")
        .context("Cannot register a clipboard format")?;
    copy_previous(html, false)?;
    let (first, rest) = {
        let _clipboard = open_clipboard()?;
        let mut formats = clipboard_win::raw::EnumFormats::new();
        (
            formats.next().context("The copy listed no format")?,
            formats,
        )
    };
    assert_eq!(
        fully_listed(std::iter::once(first).chain(rest)),
        None,
        "A format listing that failed partway was taken as complete"
    );
    let _clipboard = open_clipboard()?;
    let listed = fully_listed(clipboard_win::raw::EnumFormats::new())
        .context("A complete format listing was refused")?;
    assert!(listed.contains(&html.get()));
    Ok(())
}

fn assert_restores_only_what_it_replaced(insert: &mut Insert) -> anyhow::Result<()> {
    let html = clipboard_win::raw::register_format("HTML Format")
        .context("Cannot register a clipboard format")?;
    copy_previous(html, false)?;
    let lease = insert.acquire()?;
    let saved = insert
        .save(&lease)
        .context("Ordinary text and HTML were not saved")?;
    let mark = insert.write(lease, TEXT, Some(&saved))?;
    insert.restore(Some(saved), &mark)?;
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
    let mark = insert.write(lease, TEXT, Some(&saved))?;
    copy_without_text()?;
    insert.restore(Some(saved), &mark)?;
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
    clipboard_win::raw::set_without_clear(html.get(), b"<b>previous</b>")?;
    clipboard_win::raw::set_string_with("previous", clipboard_win::options::NoClear)?;
    if concealed {
        let format = clipboard_win::raw::register_format(CONCEALED)
            .context("Cannot register a clipboard format")?;
        clipboard_win::raw::set_without_clear(format.get(), &[0])?;
    }
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
    let word =
        |at: usize| u32::from_le_bytes(bitmap[at..at.checked_add(4).unwrap()].try_into().unwrap());
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

    let desktop = unsafe { CreateDesktopW(name.as_ptr(), null(), null(), 0, GENERIC_ALL, null()) };
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
