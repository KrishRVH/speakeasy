use std::fmt;

use anyhow::Context as _;
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
    _menu: muda::Menu,
    main: MainThreadMarker,
}

impl StatusItem {
    pub fn new(menu: muda::Menu) -> anyhow::Result<Self> {
        let main = MainThreadMarker::new().context("The menu bar item opens on the main thread")?;
        let native = unsafe { Retained::retain(menu.ns_menu().cast::<NSMenu>()) }
            .context("The menu bar item's menu has no AppKit menu")?;
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        item.setMenu(Some(&native));
        Ok(Self {
            item,
            _menu: menu,
            main,
        })
    }

    pub fn set_template_image(&self, rgba: &[u8], side: u32) -> anyhow::Result<()> {
        let png = encode_png(side, side, rgba).context("Cannot encode the menu bar image")?;
        let image = NSImage::initWithData(NSImage::alloc(), &NSData::from_vec(png))
            .context("AppKit could not read the menu bar image")?;
        let points = f64::from(side).min(MENU_BAR_POINTS);
        image.setSize(NSSize::new(points, points));
        image.setTemplate(true);
        self.button()?.setImage(Some(&image));
        Ok(())
    }

    pub fn set_tooltip(&self, tooltip: &str) -> anyhow::Result<()> {
        self.button()?
            .setToolTip(Some(&NSString::from_str(tooltip)));
        Ok(())
    }

    fn button(&self) -> anyhow::Result<Retained<NSStatusBarButton>> {
        self.item
            .button(self.main)
            .context("The menu bar item has no button")
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
