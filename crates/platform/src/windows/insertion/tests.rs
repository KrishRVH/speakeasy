use std::{
    process::Command,
    ptr::{null, null_mut},
};

use anyhow::ensure;
use windows_sys::Win32::{
    Foundation::GENERIC_ALL,
    System::StationsAndDesktops::{
        CreateDesktopW, CreateWindowStationW, GetProcessWindowStation, GetUserObjectInformationW,
        SetProcessWindowStation, SetThreadDesktop, UOI_FLAGS, USEROBJECTFLAGS,
    },
    UI::{
        Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, KEYEVENTF_UNICODE},
        WindowsAndMessaging::WSF_VISIBLE,
    },
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
        typed: insertion::KeyCount::default(),
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
#[ignore = "Starts a child on a private window station, whose clipboard is never the desktop's"]
fn copies_stay_out_of_history_and_a_restore_keeps_a_newer_copy() -> anyhow::Result<()> {
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

    Ok(())
}

#[test]
#[ignore = "Subprocess helper; does nothing unless copies_stay_out_of_history_and_a_restore_keeps_a_newer_copy starts it"]
fn check_on_a_private_window_station() -> anyhow::Result<()> {
    if std::env::var_os(PRIVATE_STATION).is_none() {
        return Ok(());
    }
    enter_private_station()?;
    copy_text(TEXT)?;
    assert_private_text("the copied text")?;
    let mut insert = insert();
    let (mark, _) = insert.copy(TEXT)?;
    assert_eq!(
        mark,
        clipboard_win::raw::seq_num(),
        "The copy's mark is not the clipboard's sequence number after the write"
    );
    assert_private_text("the pasted text")?;
    assert_private_image()?;
    assert_restores_only_what_it_replaced(&mut insert)
}

fn assert_previous(html: NonZeroU32, source: &str) -> anyhow::Result<()> {
    let _clipboard = open_clipboard()?;
    let mut text = Vec::new();
    clipboard_win::raw::get_string(&mut text)
        .with_context(|| format!("{source} lost the replaced text"))?;
    assert!(text == b"previous", "{source} lost the replaced text");
    let mut markup = Vec::new();
    clipboard_win::raw::get_vec(html.get(), &mut markup)?;
    assert!(
        markup == b"<b>previous</b>",
        "{source} lost the replaced HTML"
    );
    Ok(())
}

fn assert_restores_only_what_it_replaced(insert: &mut Insert) -> anyhow::Result<()> {
    let html = clipboard_win::raw::register_format("HTML Format")
        .context("Can't register a clipboard format")?;
    copy_previous(html)?;
    let (mark, saved) = insert.copy(TEXT)?;
    insert.restore(saved, &mark)?;
    assert_previous(html, "A restore")?;

    let (mark, saved) = insert.copy(TEXT)?;
    assert!(saved.is_some(), "The previous copy was not saved");
    copy_without_text()?;
    insert.restore(saved, &mark)?;
    let _clipboard = open_clipboard()?;
    assert!(
        clipboard_win::raw::is_format_avail(image_format()?.get()),
        "A restore replaced a newer copy"
    );
    Ok(())
}

fn copy_previous(html: NonZeroU32) -> anyhow::Result<()> {
    let _clipboard = open_clipboard()?;
    clipboard_win::raw::empty()?;
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
        assert!(
            &bytes == expected,
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
    assert!(
        colors == [&[9, 8, 7][..], &[12, 11, 10], &[3, 2, 1], &[6, 5, 4]],
        "Native bitmap conversion changed colors or orientation"
    );
    Ok(())
}

fn assert_private_text(source: &str) -> anyhow::Result<()> {
    let _clipboard = open_clipboard()?;
    let mut text = Vec::new();
    clipboard_win::raw::get_string(&mut text)?;
    assert!(
        text == b"fixture\r\ntext",
        "The clipboard lost {source} or its Windows line endings"
    );
    for name in PRIVATE_FORMATS {
        let format = clipboard_win::raw::register_format(name)
            .context("Can't register a clipboard format")?;
        let mut value = [u8::MAX; 4];
        clipboard_win::raw::get(format.get(), &mut value)
            .with_context(|| format!("{name} is missing from {source}"))?;
        assert!(value == [0; 4], "{name} lets {source} into history");
    }
    Ok(())
}

fn enter_private_station() -> anyhow::Result<()> {
    let station = unsafe { CreateWindowStationW(null(), 0, GENERIC_ALL, null()) };
    if station.is_null() {
        let refused = std::io::Error::last_os_error();
        return ensure_hidden_station()
            .with_context(|| format!("Can't create a window station: {refused}"));
    }

    ok_or_last_error(unsafe { SetProcessWindowStation(station) })?;
    ensure_hidden_station()?;
    let name = to_wide("speakeasy-test");

    let desktop = unsafe { CreateDesktopW(name.as_ptr(), null(), null(), 0, GENERIC_ALL, null()) };
    ensure!(
        !desktop.is_null(),
        "Can't create a desktop: {}",
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

fn ensure_hidden_station() -> anyhow::Result<()> {
    let mut flags = USEROBJECTFLAGS::default();
    ok_or_last_error(unsafe {
        GetUserObjectInformationW(
            GetProcessWindowStation(),
            UOI_FLAGS,
            (&raw mut flags).cast(),
            u32::try_from(size_of::<USEROBJECTFLAGS>())?,
            null_mut(),
        )
    })?;
    ensure!(
        flags.dwFlags & WSF_VISIBLE.cast_unsigned() == 0,
        "The helper's window station is interactive, so its clipboard belongs to a person"
    );
    Ok(())
}

fn image_format() -> anyhow::Result<NonZeroU32> {
    clipboard_win::raw::register_format("Speakeasy test image")
        .context("Can't register a clipboard format")
}
