use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::ensure;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString, ns_string};

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
    objc2::rc::autoreleasepool(|_| write(acquire(), text).map(|_| ()))
}

pub(super) fn write(lease: Lease, text: &str) -> anyhow::Result<isize> {
    let item = item(text, None)?;
    publish(lease, &[item])
}

pub(super) type Saved = Vec<Vec<(Retained<NSString>, Retained<NSData>)>>;

const SAVED_BYTES: usize = 64 << 20;

pub(super) fn save(lease: &Lease) -> Option<Saved> {
    let items = lease.pasteboard.pasteboardItems()?;
    let typed: Vec<_> = items.iter().map(|item| (item.types(), item)).collect();
    if typed.iter().any(|(types, _)| {
        types
            .iter()
            .any(|kind| kind.isEqualToString(ns_string!("org.nspasteboard.ConcealedType")))
    }) {
        return None;
    }
    let mut size = 0_usize;
    let mut saved = Vec::new();
    for (types, item) in typed {
        let mut kept = Vec::new();
        for kind in &types {
            let data = item.dataForType(&kind)?;
            size = size
                .checked_add(data.len())
                .filter(|size| *size <= SAVED_BYTES)?;
            kept.push((kind, data));
        }
        saved.push(kept);
    }
    (!saved.is_empty()).then_some(saved)
}

pub(super) fn restore(lease: Lease, saved: Option<&Saved>, mark: isize) -> anyhow::Result<()> {
    if lease.pasteboard.changeCount() != mark {
        return Ok(());
    }
    let Some(saved) = saved else {
        lease
            .pasteboard
            .prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
        return Ok(());
    };
    let items = saved
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
        .collect::<anyhow::Result<Vec<_>>>()?;
    publish(lease, &items).map(|_| ())
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
        crate::clipboard::copy_when(
            authorized,
            || Ok(acquire()),
            |lease| publish(lease, &[item]).map(|_| ()),
        )
    })
}

fn item(
    text: &str,
    image: Option<&crate::clipboard::Image>,
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

fn publish(lease: Lease, items: &[Retained<NSPasteboardItem>]) -> anyhow::Result<isize> {
    let writers: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = items
        .iter()
        .map(|item| ProtocolObject::from_ref(&**item))
        .collect();
    let items = NSArray::from_slice(&writers);
    let mark = lease
        .pasteboard
        .prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
    ensure!(
        lease.pasteboard.writeObjects(&items),
        "Cannot place the complete copy on the clipboard"
    );
    Ok(mark)
}
