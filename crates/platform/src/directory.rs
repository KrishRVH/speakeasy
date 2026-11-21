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
            return Err(not_a_directory());
        }
        Ok(Self(file))
    }

    pub fn child(&self, name: &OsStr) -> io::Result<Self> {
        let (file, metadata) = self.open_child(name, OpenKind::Directory)?;
        if !metadata.is_dir() {
            return Err(not_a_directory());
        }
        Ok(Self(file))
    }

    pub fn contains(&self, name: &OsStr) -> io::Result<bool> {
        entry_name(name)?;
        match native::lookup(&self.0, name) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
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
        entry_name(name)?;
        let file = native::child(&self.0, name, kind)?;
        let metadata = file.metadata()?;
        Ok((file, metadata))
    }
}

fn entry_name(name: &OsStr) -> io::Result<()> {
    if matches!(Path::new(name).components().next(), Some(Component::Normal(part)) if part == name)
    {
        Ok(())
    } else {
        Err(io::ErrorKind::InvalidInput.into())
    }
}

fn not_a_directory() -> io::Error {
    io::Error::new(io::ErrorKind::NotADirectory, "not a plain directory")
}

#[cfg(test)]
mod tests;
