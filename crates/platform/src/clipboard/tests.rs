use std::io::Cursor;

use super::*;
use crate::InputSender;

#[test]
fn png_and_bitmap_keep_channel_order_transparency_and_top_down_rows() -> anyhow::Result<()> {
    let bgra = vec![3, 2, 1, 255, 30, 20, 10, 128, 9, 8, 7, 255, 90, 80, 70, 0];
    let content = ClipboardContent::moment("PRIVATE_CONTEXT".into(), 2, 2, bgra.clone())?;
    let image = content.image.as_ref().unwrap();
    let mut decoder = png::Decoder::new(Cursor::new(&image.png)).read_info()?;
    let mut rgba = vec![0; decoder.output_buffer_size().unwrap()];
    let frame = decoder.next_frame(&mut rgba)?;
    assert_eq!((frame.width, frame.height), (2, 2));
    assert_eq!(
        rgba,
        [1, 2, 3, 255, 10, 20, 30, 128, 7, 8, 9, 255, 70, 80, 90, 0]
    );
    let word = |at| u32::from_le_bytes(image.dib[at..at + 4].try_into().unwrap());
    assert_eq!(word(0), 124);
    assert_eq!(word(4), 2);
    assert_eq!(i32::from_le_bytes(image.dib[8..12].try_into().unwrap()), -2);
    assert_eq!(word(16), 3);
    assert_eq!(
        (word(40), word(44), word(48), word(52)),
        (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000)
    );
    assert_eq!(&image.dib[124..], bgra);
    assert!(!format!("{content:?}").contains("PRIVATE_CONTEXT"));
    Ok(())
}

#[test]
fn invalid_payloads_fail_before_native_access() {
    for (width, height, pixels) in [
        (0, 1, vec![]),
        (4097, 1, vec![]),
        (1, 1, vec![0; 3]),
        (u32::MAX, u32::MAX, vec![]),
    ] {
        assert!(ClipboardContent::moment("context".into(), width, height, pixels).is_err());
    }
    assert!(ClipboardContent::text("x".repeat(TEXT_LIMIT + 1)).is_err());
    assert!(ClipboardContent::text("PRIVATE\0CONTEXT".into()).is_err());
}

#[test]
fn a_new_recording_during_acquisition_prevents_the_copy_even_after_it_finishes()
-> anyhow::Result<()> {
    let (sender, _events) = async_channel::bounded(1);
    let input = InputSender::new(sender);
    let idle = input.idle().unwrap();
    let copied = copy_when(
        || idle.is_current(),
        || {
            let recording = input.begin().unwrap();
            assert!(input.idle().is_none());
            assert!(recording.commit());
            Ok(())
        },
        |()| anyhow::bail!("An obsolete copy reached native replacement"),
    )?;
    assert!(!copied);
    let idle = input.idle().unwrap();
    input.close();
    assert!(!idle.is_current());
    assert!(input.idle().is_none());
    Ok(())
}

#[test]
fn clipboard_refusal_is_not_reported_as_a_completed_copy() {
    let result = copy_when(
        || true,
        || Ok(()),
        |()| anyhow::bail!("Clipboard refused the image"),
    );
    assert!(result.unwrap_err().to_string().contains("refused"));
}
