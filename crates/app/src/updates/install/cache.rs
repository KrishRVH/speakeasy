use std::{
    fs::{self, File, Metadata, OpenOptions},
    io,
    path::Path,
};

use anyhow::Context as _;

use super::manifest::NAME;

const CACHE_ACCESS: &str = "Cannot access the update cache. Check free space and folder access.";

pub(super) fn acquire(root: &Path) -> anyhow::Result<File> {
    fs::create_dir_all(root)
        .context("Cannot create the update cache. Check free space and folder access.")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".lock"))
        .context(CACHE_ACCESS)?;

    match lock.try_lock() {
        Ok(()) => {},
        Err(fs::TryLockError::WouldBlock) => anyhow::bail!(
            "Another Speakeasy copy is preparing an update. Finish or cancel its update first."
        ),
        Err(fs::TryLockError::Error(error)) => return Err(error).context(CACHE_ACCESS),
    }
    clean(root)
        .context("Cannot clean the update cache. Close old installers and check folder access.")?;
    Ok(lock)
}

fn clean(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)?.take(32) {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if !entry.file_name().to_string_lossy().starts_with("release-")
            || !metadata.is_dir()
            || linked(&metadata)
        {
            continue;
        }
        let mut children = fs::read_dir(&path)?;
        if let Some(child) = children.next() {
            let child = child?;
            let metadata = child.metadata()?;
            if child.file_name() != NAME
                || !metadata.is_file()
                || linked(&metadata)
                || children.next().is_some()
            {
                continue;
            }
            match fs::remove_file(child.path()) {
                Ok(()) => {},
                Err(error) if retained(&error) => continue,
                Err(error) => return Err(error),
            }
        }
        fs::remove_dir(path).or_else(|error| {
            if retained(&error) || error.kind() == io::ErrorKind::DirectoryNotEmpty {
                Ok(())
            } else {
                Err(error)
            }
        })?;
    }
    Ok(())
}

fn linked(metadata: &Metadata) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(target_os = "windows"))]
    metadata.file_type().is_symlink()
}

fn retained(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound | io::ErrorKind::WouldBlock
    ) || cfg!(target_os = "windows") && matches!(error.raw_os_error(), Some(32 | 33))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_preparation_excludes_cleanup_and_an_abandoned_stage_is_reclaimed() {
        let root = tempfile::tempdir().unwrap();
        let owner = acquire(root.path()).unwrap();
        let stage = root.path().join("release-abandoned");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join(NAME), b"synthetic installer").unwrap();
        assert!(acquire(root.path()).is_err());
        assert!(stage.join(NAME).is_file());
        drop(owner);
        let _next = acquire(root.path()).unwrap();
        assert!(!stage.exists());
        assert!(root.path().join(".lock").is_file());
    }

    #[test]
    fn cleanup_preserves_unknown_files_and_linked_content() {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("release-user-edited");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join(NAME), b"synthetic installer").unwrap();
        fs::write(stage.join("notes.txt"), b"user data").unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join(NAME), b"unowned installer").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("release-linked")).unwrap();
        let _owner = acquire(root.path()).unwrap();
        assert_eq!(fs::read(stage.join("notes.txt")).unwrap(), b"user data");
        assert!(stage.join(NAME).is_file());
        assert_eq!(
            fs::read(outside.path().join(NAME)).unwrap(),
            b"unowned installer"
        );
        #[cfg(unix)]
        assert!(root.path().join("release-linked").is_symlink());
    }
}
