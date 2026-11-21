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

#[derive(Clone, Copy)]
enum OpenKind {
    Directory,
    File,
    StableFile,
}

#[derive(Debug)]
pub struct ReadDirectory(File);

impl ReadDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = native::directory(path)?;
        if !file.metadata()?.is_dir() {
            return Err(io::Error::other("not a plain directory"));
        }
        Ok(Self(file))
    }

    pub fn child(&self, name: &OsStr) -> io::Result<Self> {
        let (file, metadata) = self.open_child(name, OpenKind::Directory)?;
        if !metadata.is_dir() {
            return Err(io::Error::other("not a plain directory"));
        }
        Ok(Self(file))
    }

    pub fn file(&self, name: &Path) -> io::Result<(File, Metadata)> {
        self.open_file(name, OpenKind::File)
    }

    pub fn stable_file(&self, name: &Path) -> io::Result<(File, Metadata)> {
        self.open_file(name, OpenKind::StableFile)
    }

    fn open_file(&self, name: &Path, kind: OpenKind) -> io::Result<(File, Metadata)> {
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
        let (file, metadata) = parent.as_ref().unwrap_or(self).open_child(name, kind)?;
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

    fn open_child(&self, name: &OsStr, kind: OpenKind) -> io::Result<(File, Metadata)> {
        if !matches!(Path::new(name).components().next(), Some(Component::Normal(part)) if part == name)
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let file = native::child(&self.0, name, kind)?;
        let metadata = file.metadata()?;
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
mod tests;
