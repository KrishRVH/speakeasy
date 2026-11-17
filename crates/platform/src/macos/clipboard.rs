use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::ensure;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString};

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
    objc2::rc::autoreleasepool(|_| write(acquire(), text))
}

pub(super) fn write(lease: Lease, text: &str) -> anyhow::Result<()> {
    let item = item(text, None)?;
    publish(lease, &item)
}

pub(crate) fn copy_content(
    content: &crate::ClipboardContent,
    authorized: impl FnOnce() -> bool,
) -> anyhow::Result<bool> {
    objc2::rc::autoreleasepool(|_| {
        let item = item(&content.text, content.image.as_ref())?;
        crate::clipboard::copy_when(authorized, || Ok(acquire()), |lease| publish(lease, &item))
    })
}

fn item(
    text: &str,
    image: Option<&crate::clipboard::Image>,
) -> anyhow::Result<Retained<NSPasteboardItem>> {
    let item = NSPasteboardItem::new();
    ensure!(
        item.setString_forType(
            &NSString::new(),
            &NSString::from_str("org.nspasteboard.TransientType")
        ),
        "Cannot mark the clipboard copy private"
    );
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

fn publish(lease: Lease, item: &NSPasteboardItem) -> anyhow::Result<()> {
    let item: &ProtocolObject<dyn NSPasteboardWriting> = ProtocolObject::from_ref(item);
    let items = NSArray::from_slice(&[item]);
    lease
        .pasteboard
        .prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
    ensure!(
        lease.pasteboard.writeObjects(&items),
        "Cannot place the complete copy on the clipboard"
    );
    Ok(())
}
