use std::{
    fs, io,
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

pub(crate) fn managed_id(parent: &Path, directory: &Path) -> io::Result<Option<ShowId>> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let Some(id) = directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(ShowId::parse)
    else {
        return Ok(None);
    };
    let Some(container) = directory.parent() else {
        return Ok(None);
    };
    let managed = match parent.canonicalize() {
        Ok(managed) => managed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    Ok((container.canonicalize()? == managed
        && directory.canonicalize()? == managed.join(id.as_str()))
    .then_some(id))
}
