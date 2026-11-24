use std::fmt;

use anyhow::{Context as _, bail};
use muda::ContextMenu as _;
use objc2::{AllocAnyThread as _, MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSImage, NSMenu, NSStatusBar, NSStatusBarButton, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSSize, NSString};

use crate::image::encode_png;

const MENU_BAR_POINTS: f64 = 22.0;

pub struct StatusItem {
    item: Retained<NSStatusItem>,
    button: Retained<NSStatusBarButton>,
    /// Dropping the muda menu cancels `AppKit`'s tracking of it, so it lives as long as the item.
    _menu: muda::Menu,
}

impl StatusItem {
    pub fn new(menu: muda::Menu) -> anyhow::Result<Self> {
        let main = MainThreadMarker::new().context("The menu bar item opens on the main thread")?;
        // SAFETY: `ns_menu` points at the `NSMenu` that `menu` retains; this adds our own
        // reference.
        let native = unsafe { Retained::retain(menu.ns_menu().cast::<NSMenu>()) }
            .context("The menu bar item's menu has no AppKit menu")?;
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        let Some(button) = item.button(main) else {
            bar.removeStatusItem(&item);
            bail!("The menu bar item has no button");
        };
        item.setMenu(Some(&native));
        Ok(Self {
            item,
            button,
            _menu: menu,
        })
    }

    pub fn set_template_image(&self, rgba: &[u8], side: u32) -> anyhow::Result<()> {
        let png = encode_png(side, side, rgba).context("Can't encode the menu bar image")?;
        let image = NSImage::initWithData(NSImage::alloc(), &NSData::from_vec(png))
            .context("AppKit could not read the menu bar image")?;
        let points = f64::from(side).min(MENU_BAR_POINTS);
        image.setSize(NSSize::new(points, points));
        image.setTemplate(true);
        self.button.setImage(Some(&image));
        Ok(())
    }

    pub fn set_tooltip(&self, tooltip: &str) {
        self.button.setToolTip(Some(&NSString::from_str(tooltip)));
    }
}

impl Drop for StatusItem {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}

impl fmt::Debug for StatusItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatusItem").finish_non_exhaustive()
    }
}
