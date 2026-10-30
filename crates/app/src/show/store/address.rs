//! Exact Show identities. A stamp resolves only in managed storage; explicit paths stay paths.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// The complete name of one managed Show, including a same-second suffix if present.
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

/// A bare stamp is an ID. Every other argument is a path; `./ID` explicitly names a relative path
/// even when a managed Show has the same name.
pub(crate) fn resolve(
    target: &Path,
    parent: impl FnOnce() -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    if let Some(id) = target.to_str().and_then(ShowId::parse) {
        return Ok(parent()?.join(id.as_str()));
    }
    std::path::absolute(target)
}

/// Returns an ID only for a real directory directly in `parent`. A moved or linked bundle must
/// never advertise an ID that addresses a different Show in managed storage.
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
    // A trailing separator makes symlink_metadata follow a directory link on Unix. Confirm the
    // resolved directory too, so spelling a linked bundle as `ID/` cannot bypass the type check.
    Ok((container.canonicalize()? == managed
        && directory.canonicalize()? == managed.join(id.as_str()))
    .then_some(id))
}
