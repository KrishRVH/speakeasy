use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::{anyhow, ensure};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString, ns_string};

use crate::clipboard::{self, Screen};

#[expect(
    clippy::disallowed_types,
    reason = "The insertion thread and explicit-copy worker each own native calls, including copies while input is stopped; this lease serializes their pasteboard transactions, not session state"
)]
static ACCESS: Mutex<()> = Mutex::new(());

pub(super) struct Lease {
    pasteboard: Retained<NSPasteboard>,
    _access: MutexGuard<'static, ()>,
}

pub(super) fn acquire() -> Lease {
    let access = ACCESS.lock().unwrap_or_else(PoisonError::into_inner);
    Lease {
        pasteboard: NSPasteboard::generalPasteboard(),
        _access: access,
    }
}

pub(crate) fn copy_text(text: &str) -> anyhow::Result<()> {
    objc2::rc::autoreleasepool(|_| write(acquire(), text, None).map(|_| ()))
}

pub(super) fn write(lease: Lease, text: &str, previous: Option<&Saved>) -> anyhow::Result<isize> {
    let item = item(text, None)?;
    publish(&lease, &[item], previous)
}

pub(super) type Saved = Vec<Vec<(Retained<NSString>, Retained<NSData>)>>;

const SAVED_BYTES: usize = 64 << 20;

pub(super) fn save(lease: &Lease) -> Option<Saved> {
    let items = lease.pasteboard.pasteboardItems()?;
    let mut size = 0_usize;
    clipboard::snapshot(
        items.iter().map(|item| (item.types(), item)),
        |(types, _)| {
            if concealed(types) {
                Screen::Refuse
            } else {
                Screen::Save
            }
        },
        |(types, item)| {
            types
                .iter()
                .map(|kind| {
                    let data = item.dataForType(&kind)?;
                    size = size
                        .checked_add(data.len())
                        .filter(|size| *size <= SAVED_BYTES)?;
                    Some((kind, data))
                })
                .collect()
        },
    )
}

pub(super) fn holds(lease: &Lease, text: &str, mark: isize) -> bool {
    let unchanged = || lease.pasteboard.changeCount() == mark;
    let string = unsafe { NSPasteboardTypeString };
    unchanged()
        && lease
            .pasteboard
            .pasteboardItems()
            .filter(|items| items.count() == 1)
            .and_then(|items| items.firstObject())
            .is_some_and(|item| {
                !concealed(&item.types())
                    && item
                        .stringForType(string)
                        .is_some_and(|held| held.isEqualToString(&NSString::from_str(text)))
            })
        && unchanged()
}

fn concealed(types: &NSArray<NSString>) -> bool {
    types
        .iter()
        .any(|kind| kind.isEqualToString(ns_string!("org.nspasteboard.ConcealedType")))
}

pub(super) fn restore(lease: Lease, saved: Option<&Saved>, mark: isize) -> anyhow::Result<()> {
    let items = saved.map(items_from).transpose()?;
    if lease.pasteboard.changeCount() != mark {
        return Ok(());
    }
    let Some(items) = items else {
        lease
            .pasteboard
            .prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
        return Ok(());
    };
    publish(&lease, &items, None).map(|_| ())
}

fn items_from(saved: &Saved) -> anyhow::Result<Vec<Retained<NSPasteboardItem>>> {
    saved
        .iter()
        .map(|kept| {
            let item = private_item()?;
            for (kind, data) in kept {
                ensure!(
                    item.setData_forType(data, kind),
                    "Cannot prepare the previous clipboard"
                );
            }
            Ok(item)
        })
        .collect()
}

fn private_item() -> anyhow::Result<Retained<NSPasteboardItem>> {
    let item = NSPasteboardItem::new();
    ensure!(
        item.setString_forType(ns_string!(""), ns_string!("org.nspasteboard.TransientType")),
        "Cannot mark the clipboard copy private"
    );
    Ok(item)
}

pub(crate) fn copy_content(
    content: &crate::ClipboardContent,
    authorized: impl FnOnce() -> bool,
) -> anyhow::Result<bool> {
    objc2::rc::autoreleasepool(|_| {
        let item = item(&content.text, content.image.as_ref())?;
        clipboard::copy_when(
            authorized,
            || Ok(acquire()),
            |lease| publish(&lease, &[item], None).map(|_| ()),
        )
    })
}

fn item(
    text: &str,
    image: Option<&clipboard::Image>,
) -> anyhow::Result<Retained<NSPasteboardItem>> {
    let item = private_item()?;
    if let Some(image) = image {
        let png = unsafe { NSPasteboardTypePNG };
        ensure!(
            item.setData_forType(&NSData::with_bytes(&image.png), png),
            "Cannot prepare the clipboard image"
        );
    }

    let string = unsafe { NSPasteboardTypeString };
    ensure!(
        item.setString_forType(&NSString::from_str(text), string),
        "Cannot prepare the clipboard text"
    );
    Ok(item)
}

fn publish(
    lease: &Lease,
    items: &[Retained<NSPasteboardItem>],
    previous: Option<&Saved>,
) -> anyhow::Result<isize> {
    let writers: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = items
        .iter()
        .map(|item| ProtocolObject::from_ref(&**item))
        .collect();
    let items = NSArray::from_slice(&writers);
    let cleared = lease
        .pasteboard
        .prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
    if lease.pasteboard.writeObjects(&items) {
        return Ok(lease.pasteboard.changeCount());
    }
    if let Some(previous) = previous
        && lease.pasteboard.changeCount() == cleared
    {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Putting the previous copy back is best effort; the failed write is the error to report"
        )]
        let _ = items_from(previous).and_then(|items| publish(lease, &items, None));
    }
    Err(anyhow!("Cannot place the complete copy on the clipboard"))
}
