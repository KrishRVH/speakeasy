//! Assets embedded in the binary and served to GPUI.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub(crate) const KEYSTONE: &str = "icons/keystone.svg";
pub(crate) const MICROPHONE: &str = "icons/symbols/microphone.svg";
pub(crate) const LOCK: &str = "icons/symbols/lock.svg";
pub(crate) const SCREEN: &str = "icons/symbols/screen.svg";
pub(crate) const CHECK: &str = "icons/symbols/check.svg";
pub(crate) const ATTENTION: &str = "icons/symbols/attention.svg";

const ASSETS: [(&str, &[u8]); 6] = [
    (KEYSTONE, include_bytes!("../assets/keystone.svg")),
    (
        MICROPHONE,
        include_bytes!("../assets/symbols/microphone.svg"),
    ),
    (LOCK, include_bytes!("../assets/symbols/lock.svg")),
    (SCREEN, include_bytes!("../assets/symbols/screen.svg")),
    (CHECK, include_bytes!("../assets/symbols/check.svg")),
    (ATTENTION, include_bytes!("../assets/symbols/attention.svg")),
];

pub(crate) struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS
            .iter()
            .find(|(asset, _)| *asset == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ASSETS
            .iter()
            .filter(|(asset, _)| asset.starts_with(path))
            .map(|(asset, _)| SharedString::from(*asset))
            .collect())
    }
}
