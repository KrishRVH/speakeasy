use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io,
    path::{Component, Path},
};

#[cfg(unix)]
#[path = "directory/unix.rs"]
mod native;
#[cfg(windows)]
#[path = "directory/windows.rs"]
mod native;

#[derive(Debug)]
pub struct ReadDirectory(File);

impl ReadDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = native::directory(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || native::linked(&metadata) {
            return Err(io::Error::other("not a plain directory"));
        }
        Ok(Self(file))
    }

    pub fn child(&self, name: &OsStr) -> io::Result<Self> {
        let (file, metadata) = self.open_child(name, true)?;
        if !metadata.is_dir() {
            return Err(io::Error::other("not a plain directory"));
        }
        Ok(Self(file))
    }

    pub fn file(&self, name: &Path) -> io::Result<(File, Metadata)> {
        let mut parts = name.components();
        let Some(Component::Normal(name)) = parts.next_back() else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        let parent = parts.try_fold(None, |parent: Option<Self>, part| {
            let Component::Normal(folder) = part else {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            };
            parent.as_ref().unwrap_or(self).child(folder).map(Some)
        })?;
        let (file, metadata) = parent.as_ref().unwrap_or(self).open_child(name, false)?;
        if !metadata.is_file() {
            return Err(io::Error::other("not a regular file"));
        }
        Ok((file, metadata))
    }

    pub fn is_at(&self, path: &Path) -> io::Result<bool> {
        native::same(&self.0, &Self::open(path)?.0)
    }

    pub fn visit(&self, visit: impl FnMut(OsString)) -> io::Result<()> {
        native::visit(&self.0, visit)
    }

    pub fn duplicate(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    fn open_child(&self, name: &OsStr, directory: bool) -> io::Result<(File, Metadata)> {
        if !matches!(Path::new(name).components().next(), Some(Component::Normal(part)) if part == name)
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let file = native::child(&self.0, name, directory)?;
        let metadata = file.metadata()?;
        if native::linked(&metadata) {
            return Err(io::Error::other("linked entry"));
        }
        Ok((file, metadata))
    }
}

pub fn open_lock(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

        let file = options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::Error::other("lock must be a private regular file"));
        }
        if metadata.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(file)
    }
    #[cfg(windows)]
    options.open(path)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn reads_keep_the_open_directory_across_a_rename() -> anyhow::Result<()> {
        use std::io::Read as _;

        let root = tempfile::tempdir()?;
        let original = root.path().join("original");
        fs::create_dir(&original)?;
        fs::write(original.join("public.txt"), "original")?;
        let opened = ReadDirectory::open(&original)?;
        fs::rename(&original, root.path().join("renamed"))?;
        fs::create_dir(&original)?;
        fs::write(original.join("public.txt"), "replacement")?;
        fs::write(original.join("new.txt"), "replacement")?;
        assert!(!opened.is_at(&original)?);
        let mut entries = Vec::new();
        for _ in 0..2 {
            opened.visit(|name| {
                if name != "." && name != ".." {
                    entries.push(name);
                }
            })?;
        }
        assert_eq!(entries, vec![OsString::from("public.txt"); 2]);
        let (mut file, _) = opened.file(Path::new("public.txt"))?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        assert_eq!(text, "original");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() -> anyhow::Result<()> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

        let root = tempfile::tempdir()?;
        let fifo = CString::new(root.path().join("fifo").as_os_str().as_bytes())?;
        if unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let directory = ReadDirectory::open(root.path())?;
        assert!(directory.file(Path::new("fifo")).is_err());
        Ok(())
    }
}
