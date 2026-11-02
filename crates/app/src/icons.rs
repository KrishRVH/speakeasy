use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub(crate) const KEYSTONE: &str = "icons/keystone.svg";
pub(crate) const JEWEL_HOLLOW: &str = "icons/jewel-hollow.svg";
pub(crate) const JEWEL_CUT: &str = "icons/jewel-cut.svg";
pub(crate) const MICROPHONE: &str = "icons/symbols/microphone.svg";
pub(crate) const LOCK: &str = "icons/symbols/lock.svg";
pub(crate) const SCREEN: &str = "icons/symbols/screen.svg";
pub(crate) const CHECK: &str = "icons/symbols/check.svg";
pub(crate) const CHEVRON_DOWN: &str = "icons/symbols/chevron-down.svg";
pub(crate) const CHEVRON_RIGHT: &str = "icons/symbols/chevron-right.svg";
pub(crate) const CHEVRON_LEFT: &str = "icons/symbols/chevron-left.svg";
pub(crate) const CLOSE: &str = "icons/symbols/close.svg";
pub(crate) const PLUS: &str = "icons/symbols/plus.svg";
pub(crate) const MORE: &str = "icons/symbols/more.svg";
pub(crate) const ARROW_RIGHT: &str = "icons/symbols/arrow-right.svg";
pub(crate) const INFO: &str = "icons/symbols/info.svg";

const ASSETS: &[(&str, &[u8])] = &[
    (KEYSTONE, include_bytes!("../assets/keystone.svg")),
    (JEWEL_HOLLOW, include_bytes!("../assets/jewel-hollow.svg")),
    (JEWEL_CUT, include_bytes!("../assets/jewel-cut.svg")),
    (
        MICROPHONE,
        include_bytes!("../assets/symbols/microphone.svg"),
    ),
    (LOCK, include_bytes!("../assets/symbols/lock.svg")),
    (SCREEN, include_bytes!("../assets/symbols/screen.svg")),
    (CHECK, include_bytes!("../assets/symbols/check.svg")),
    (
        CHEVRON_DOWN,
        include_bytes!("../assets/symbols/chevron-down.svg"),
    ),
    (
        CHEVRON_RIGHT,
        include_bytes!("../assets/symbols/chevron-right.svg"),
    ),
    (
        CHEVRON_LEFT,
        include_bytes!("../assets/symbols/chevron-left.svg"),
    ),
    (CLOSE, include_bytes!("../assets/symbols/close.svg")),
    (PLUS, include_bytes!("../assets/symbols/plus.svg")),
    (MORE, include_bytes!("../assets/symbols/more.svg")),
    (
        ARROW_RIGHT,
        include_bytes!("../assets/symbols/arrow-right.svg"),
    ),
    (INFO, include_bytes!("../assets/symbols/info.svg")),
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
