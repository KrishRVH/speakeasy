use anyhow::ensure;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString, ns_string};

use crate::clipboard;

const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

const INCOMPLETE: &str = "Cannot place the complete copy on the clipboard";

pub(super) type Saved = Vec<Vec<(Retained<NSString>, Retained<NSData>)>>;

pub(crate) fn copy_text(text: &str) -> anyhow::Result<()> {
    objc2::rc::autoreleasepool(|_| write(text).map(|_| ()))
}

pub(super) fn write(text: &str) -> anyhow::Result<isize> {
    publish(&writers(&[item(text, None)?]))
}

pub(super) fn snapshot() -> Option<Saved> {
    let saved = NSPasteboard::generalPasteboard()
        .pasteboardItems()?
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .map(|kind| item.dataForType(&kind).map(|data| (kind, data)))
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Saved>>()?;
    (!saved.is_empty()).then_some(saved)
}

pub(super) fn restore(saved: Option<&Saved>, mark: isize) -> anyhow::Result<()> {
    let items = saved.map(items_from).transpose()?;
    let pasteboard = NSPasteboard::generalPasteboard();
    if pasteboard.changeCount() != mark {
        return Ok(());
    }
    clear(&pasteboard);
    if let Some(items) = items {
        ensure!(pasteboard.writeObjects(&writers(&items)), INCOMPLETE);
    }
    Ok(())
}

pub(crate) fn copy_content(
    content: &crate::ClipboardContent,
    authorized: impl FnOnce() -> bool,
) -> anyhow::Result<bool> {
    objc2::rc::autoreleasepool(|_| {
        let items = writers(&[item(&content.text, content.image.as_ref())?]);
        if !authorized() {
            return Ok(false);
        }
        publish(&items)?;
        Ok(true)
    })
}

fn publish(items: &Writers) -> anyhow::Result<isize> {
    let pasteboard = NSPasteboard::generalPasteboard();
    clear(&pasteboard);
    ensure!(pasteboard.writeObjects(items), INCOMPLETE);
    Ok(pasteboard.changeCount())
}

fn clear(pasteboard: &NSPasteboard) {
    pasteboard.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
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
        item.setString_forType(ns_string!(""), &NSString::from_str(TRANSIENT_TYPE)),
        "Cannot mark the clipboard copy private"
    );
    Ok(item)
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

type Writers = Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>>;

fn writers(items: &[Retained<NSPasteboardItem>]) -> Writers {
    let writers: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = items
        .iter()
        .map(|item| ProtocolObject::from_ref(&**item))
        .collect();
    NSArray::from_slice(&writers)
}
