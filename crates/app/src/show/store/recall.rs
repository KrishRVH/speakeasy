use std::{
    fmt,
    fs::{self, File},
    io::{self, Read as _},
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use speakeasy_platform::ReadDirectory;
use speakeasy_show::{Bundle, Manifest};

#[cfg(any(target_os = "windows", target_os = "macos", test))]
use super::SavedShow;
use super::{
    address::{self, ShowId},
    found, movie, stamp,
};

#[derive(Debug)]
pub(crate) struct ReadShow {
    pub manifest: Manifest,
    pub movie: Option<movie::Inspected>,
}

#[derive(Debug)]
pub(crate) struct Entry {
    pub directory: PathBuf,
    pub show: anyhow::Result<ReadShow>,
    pub pinned: bool,
    pub id: Option<ShowId>,
}

impl Entry {
    pub(crate) fn open(directory: &Path, managed: Option<&Path>) -> io::Result<Self> {
        let directory = std::path::absolute(directory)?;
        if !fs::metadata(&directory)?.is_dir() {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        Ok(Self {
            show: read(&directory),
            pinned: super::pinned(&directory).unwrap_or(false),
            id: managed.and_then(|parent| address::managed_id(parent, &directory)),
            directory,
        })
    }

    pub(crate) fn file(&self, name: &str) -> PathBuf {
        self.directory.join(name.split('/').collect::<PathBuf>())
    }
}

fn read(directory: &Path) -> anyhow::Result<ReadShow> {
    let bytes = fs::read(directory.join(Manifest::FILE))
        .with_context(|| format!("Cannot read {}", Manifest::FILE))?;
    let manifest = Manifest::parse(&bytes)?;
    let movie = movie::saved(directory, manifest.recording().duration)
        .with_context(|| format!("Cannot read {}", movie::FILE))?;
    Ok(ReadShow { manifest, movie })
}

pub(crate) fn list(parent: &Path) -> io::Result<impl Iterator<Item = io::Result<Entry>>> {
    Ok(directories(parent)?
        .into_iter()
        .filter_map(
            move |directory| match Entry::open(&directory, Some(parent)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                entry => Some(entry),
            },
        ))
}

pub(crate) fn directories(parent: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(entries) = found(fs::read_dir(parent))? else {
        return Ok(Vec::new());
    };
    let mut shows = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some((name, stamp)) = name
            .to_str()
            .and_then(|name| stamp(name).map(|stamp| (name, stamp)))
        else {
            continue;
        };
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let suffix = name
            .split_once('-')
            .map_or(1, |(_, number)| number.parse::<u64>().unwrap_or(u64::MAX));
        shows.push((stamp.to_owned(), suffix, entry.path()));
    }
    shows.sort_by(|a, b| b.cmp(a));
    Ok(shows.into_iter().map(|(_, _, entry)| entry).collect())
}

pub(crate) fn summary(directory: &Path) -> io::Result<String> {
    fs::read_to_string(directory.join(Bundle::SUMMARY))
}

pub(in crate::show::store) fn open_directory(directory: &Path) -> io::Result<ReadDirectory> {
    ReadDirectory::open(directory)
}

pub(super) fn open_file(directory: &ReadDirectory, name: &str, limit: u64) -> io::Result<File> {
    let (file, metadata) = directory.file(Path::new(name))?;
    if metadata.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "file is too large",
        ));
    }
    if metadata.len() == 0 {
        return Err(io::Error::other(Empty));
    }
    Ok(file)
}

pub(super) fn read_file(directory: &Path, name: &str, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_file(&open_directory(directory)?, name, limit)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(in crate::show::store) fn empty(error: &io::Error) -> bool {
    matches!(error.get_ref(), Some(inner) if inner.is::<Empty>())
}

#[derive(Debug)]
struct Empty;

impl fmt::Display for Empty {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("file is empty")
    }
}

impl std::error::Error for Empty {}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl SavedShow {
    pub(crate) fn folder(&self) -> anyhow::Result<Option<&Path>> {
        let metadata = found(fs::metadata(&self.0)).map_err(cannot_read(&self.0))?;
        Ok(metadata
            .is_some_and(|metadata| metadata.is_dir())
            .then_some(self.directory()))
    }

    pub(crate) fn summary(&self) -> anyhow::Result<Option<String>> {
        found(summary(&self.0)).map_err(cannot_read(&self.0.join(Bundle::SUMMARY)))
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
fn cannot_read(path: &Path) -> impl FnOnce(io::Error) -> anyhow::Error {
    move |error| anyhow::anyhow!("Cannot read {}: {error}.", path.display())
}

#[cfg(test)]
mod tests;
