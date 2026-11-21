use std::{cell::Cell, io::Cursor};

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
    assert!(ClipboardContent::text("x".repeat(ClipboardContent::MAX_TEXT_BYTES + 1)).is_err());
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
fn a_snapshot_screens_every_kind_before_reading_any() {
    let screen = |kind: &&str| match *kind {
        "concealed" => Screen::Refuse,
        "marker" => Screen::Skip,
        _ => Screen::Save,
    };
    let mut read = Vec::new();
    let concealed = snapshot(["text", "html", "concealed"], screen, |kind| {
        read.push(kind);
        Some(kind)
    });
    assert_eq!(concealed, None);
    assert!(
        read.is_empty(),
        "A concealed copy was read before its last format was screened"
    );

    let saved = snapshot(["text", "marker", "html"], screen, |kind| {
        read.push(kind);
        Some(kind.len())
    });
    assert_eq!(saved, Some(vec![4, 4]));
    assert_eq!(read, ["text", "html"], "A skipped format was read");

    let unreadable = snapshot(["text", "html"], screen, |kind| {
        (kind == "text").then_some(kind)
    });
    assert_eq!(unreadable, None, "A partial copy was kept to restore");
    assert_eq!(snapshot(["marker"], screen, Some), None);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Previous,
    Dictation,
    Empty,
    Newer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Snapshot,
    Prepare,
}

#[expect(
    clippy::disallowed_types,
    reason = "The fake stands in for AppKit's general pasteboard, which other apps change behind the lease, so its methods take &self like NSPasteboard's"
)]
struct Board {
    count: Cell<isize>,
    held: Cell<Held>,
    failing_writes: Cell<u8>,
    copies_during: Option<Phase>,
}

impl Board {
    #[expect(
        clippy::disallowed_types,
        reason = "Builds the cells through which the fake pasteboard changes behind a shared borrow, as another app changes AppKit's"
    )]
    fn new(failing_writes: u8, copies_during: Option<Phase>) -> Self {
        Self {
            count: Cell::new(1),
            held: Cell::new(Held::Previous),
            failing_writes: Cell::new(failing_writes),
            copies_during,
        }
    }

    fn change(&self, held: Held) -> isize {
        self.count.set(self.count.get().wrapping_add(1));
        self.held.set(held);
        self.count.get()
    }

    fn copies_during(&self, phase: Phase) {
        if self.copies_during == Some(phase) {
            self.change(Held::Newer);
        }
    }
}

impl Pasteboard for Board {
    type Items = Held;
    type Saved = Held;

    fn snapshot(&self) -> Option<Held> {
        let read = self.held.get();
        self.copies_during(Phase::Snapshot);
        Some(read)
    }

    fn prepare(&self, saved: &Held) -> anyhow::Result<Held> {
        self.copies_during(Phase::Prepare);
        Ok(*saved)
    }

    fn change_count(&self) -> isize {
        self.count.get()
    }

    fn clear(&self) -> isize {
        self.change(Held::Empty)
    }

    fn write(&self, items: &Held) -> bool {
        let failing = self.failing_writes.get();
        if failing > 0 {
            self.failing_writes.set(failing.saturating_sub(1));
            return false;
        }
        self.held.set(*items);
        true
    }
}

#[test]
fn a_failed_pasteboard_write_puts_back_only_what_no_one_has_replaced() {
    let board = Board::new(1, None);
    assert!(publish(&board, &Held::Dictation, Some(&Held::Previous)).is_err());
    assert_eq!(board.held.get(), Held::Previous);

    let board = Board::new(1, Some(Phase::Prepare));
    assert!(publish(&board, &Held::Dictation, Some(&Held::Previous)).is_err());
    assert_eq!(
        board.held.get(),
        Held::Newer,
        "A copy made while the previous one was rebuilt was cleared"
    );

    let board = Board::new(0, Some(Phase::Prepare));
    let mark = publish(&board, &Held::Dictation, None).unwrap();
    restore(&board, Some(&Held::Previous), mark).unwrap();
    assert_eq!(
        board.held.get(),
        Held::Newer,
        "A restore replaced a copy made while it was rebuilt"
    );
}

#[test]
fn a_pasteboard_snapshot_that_changed_while_it_was_read_is_dropped() {
    assert_eq!(save(&Board::new(0, None)), Some(Held::Previous));
    assert_eq!(
        save(&Board::new(0, Some(Phase::Snapshot))),
        None,
        "A snapshot outlived by a newer copy would be restored over it"
    );
}
