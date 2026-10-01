use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

pub(crate) const KEYSTONE: &str = "icons/keystone.svg";

pub(crate) struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok((path == KEYSTONE)
            .then(|| Cow::Borrowed(include_bytes!("../assets/keystone.svg").as_slice())))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(if KEYSTONE.starts_with(path) {
            vec![KEYSTONE.into()]
        } else {
            Vec::new()
        })
    }
}
