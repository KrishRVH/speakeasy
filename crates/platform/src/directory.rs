use std::{
    fs::{File, Metadata},
    io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct ReadDirectory(PathBuf);

impl ReadDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.metadata()?.is_dir() {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        Ok(Self(path.to_owned()))
    }

    pub fn file(&self, name: &Path) -> io::Result<(File, Metadata)> {
        let file = File::open(self.0.join(name))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::other("not a regular file"));
        }
        Ok((file, metadata))
    }
}
