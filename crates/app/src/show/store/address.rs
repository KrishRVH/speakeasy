use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShowId(String);

impl ShowId {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        super::stamp(name)?;
        Some(Self(name.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn resolve(
    target: &Path,
    parent: impl FnOnce() -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    if let Some(id) = target.to_str().and_then(ShowId::parse) {
        return Ok(parent()?.join(id.as_str()));
    }
    std::path::absolute(target)
}

pub(crate) fn managed_id(parent: &Path, directory: &Path) -> Option<ShowId> {
    let id = ShowId::parse(directory.file_name()?.to_str()?)?;
    (directory.parent()? == parent).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_ids_never_fall_back_to_relative_paths() -> anyhow::Result<()> {
        let parent = Path::new("/managed");
        let id = "20261030T101010Z-2";
        assert_eq!(
            resolve(Path::new(id), || Ok(parent.into()))?,
            parent.join(id)
        );
        let explicit = format!("./{id}");
        assert_eq!(
            resolve(Path::new(&explicit), || panic!(
                "explicit path consulted storage"
            ))?,
            std::path::absolute(&explicit)?
        );
        for invalid in [
            "latest",
            "../20261030T101010Z",
            "20261030T101010Z/",
            "20261030T101010Z-a",
        ] {
            assert!(ShowId::parse(invalid).is_none());
        }
        Ok(())
    }
}
