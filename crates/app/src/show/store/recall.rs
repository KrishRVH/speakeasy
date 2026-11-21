use std::{
    fmt,
    fs::{self, File},
    io::{self, Read as _},
    path::{Path, PathBuf},
};

use serde::Serialize;
use speakeasy_platform::ReadDirectory;
use speakeasy_show::{Bundle, Manifest, Unreadable};

#[cfg(any(target_os = "windows", target_os = "macos", test))]
use super::SavedShow;
use super::{PIN, found, stamp};

pub(super) const SUMMARY_LIMIT: u64 = 1 << 20;

pub(super) const MANIFEST_LIMIT: u64 = 16 << 20;

pub(crate) const IMAGE_LIMIT: u64 = 32 << 20;

#[derive(Debug)]
pub(crate) struct ReadShow {
    pub manifest: Manifest,
    pub pinned: bool,
    pub problems: Vec<FileProblem>,
    pub movie: Option<super::movie::Inspected>,
}

#[derive(Debug)]
pub(crate) struct FileProblem {
    pub name: String,
    pub reason: String,
}

#[derive(Debug)]
pub(crate) enum ReadError {
    Manifest(Unreadable),
    File(FileProblem),
}

impl fmt::Display for ReadError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => error.fmt(out),
            Self::File(problem) => write!(out, "{}: {}", problem.name, problem.reason),
        }
    }
}

impl std::error::Error for ReadError {}

#[derive(Debug)]
pub(crate) struct Entry {
    pub directory: PathBuf,
    pub show: Result<ReadShow, ReadError>,
    pinned: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Health {
    Ok,
    Damaged,
    Unsupported,
}

impl Entry {
    pub(crate) fn health(&self) -> Health {
        match &self.show {
            Ok(show) if show.problems.is_empty() => Health::Ok,
            Err(ReadError::Manifest(Unreadable::Unsupported(_))) => Health::Unsupported,
            _ => Health::Damaged,
        }
    }

    pub(crate) const fn pinned(&self) -> Option<bool> {
        self.pinned
    }

    pub(crate) fn open(directory: &Path) -> io::Result<Self> {
        let directory = std::path::absolute(directory)?;
        let files = open_directory(&directory)?;
        Ok(Self::read_from(directory, &files))
    }

    pub(in crate::show) fn read_from(directory: PathBuf, files: &ReadDirectory) -> Self {
        let pinned = kept(files);
        let show = read(files, pinned.as_ref().copied());
        Self {
            directory,
            show,
            pinned: pinned.ok(),
        }
    }

    pub(crate) fn file(&self, name: &str) -> PathBuf {
        bundle_path(&self.directory, name)
    }
}

pub(crate) fn list(parent: &Path) -> io::Result<impl Iterator<Item = io::Result<Entry>>> {
    let paths = directories(parent)?;
    Ok(paths.into_iter().filter_map(|path| {
        found(Entry::open(&path).and_then(|entry| {
            fs::metadata(&entry.directory)?;
            Ok(entry)
        }))
        .transpose()
    }))
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
        if !found(entry.file_type())?.is_some_and(|kind| kind.is_dir()) {
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

fn read(
    directory: &ReadDirectory,
    pinned: Result<bool, &io::Error>,
) -> Result<ReadShow, ReadError> {
    let file_error = |name: &str, error: &io::Error| {
        ReadError::File(FileProblem {
            name: name.to_owned(),
            reason: error.to_string(),
        })
    };
    let bytes = read_from(directory, Manifest::FILE, MANIFEST_LIMIT)
        .map_err(|error| file_error(Manifest::FILE, &error))?;
    let manifest = Manifest::parse(&bytes).map_err(ReadError::Manifest)?;
    let pinned = pinned.map_err(|error| file_error(PIN, error))?;
    let mut problems: Vec<_> = manifest
        .files()
        .filter_map(|name| {
            let limit = if name == Bundle::SUMMARY {
                SUMMARY_LIMIT
            } else {
                u64::MAX
            };
            open_file(directory, &name, limit)
                .err()
                .map(|error| FileProblem {
                    name,
                    reason: error.to_string(),
                })
        })
        .collect();
    let movie = match super::movie::saved(directory, manifest.recording().duration) {
        Ok(movie) => movie,
        Err(error) => {
            problems.push(FileProblem {
                name: super::movie::FILE.to_owned(),
                reason: error.to_string(),
            });
            None
        },
    };
    Ok(ReadShow {
        manifest,
        pinned,
        problems,
        movie,
    })
}

fn kept(directory: &ReadDirectory) -> io::Result<bool> {
    let unopened = match found(directory.file(Path::new(PIN))) {
        Ok(marker) => return Ok(marker.is_some()),
        Err(error) => error,
    };
    let mut listed = false;
    directory.visit(|name| listed |= name == PIN)?;
    if listed { Ok(true) } else { Err(unopened) }
}

pub(in crate::show::store) fn open_directory(directory: &Path) -> io::Result<ReadDirectory> {
    ReadDirectory::open(&directory.canonicalize()?)
}

pub(super) fn regular_file(directory: &Path, name: &str, limit: u64) -> io::Result<()> {
    open_file(&open_directory(directory)?, name, limit).map(drop)
}

pub(super) fn open_file(directory: &ReadDirectory, name: &str, limit: u64) -> io::Result<File> {
    let (file, metadata) = directory.file(Path::new(name))?;
    if metadata.len() > limit {
        return Err(too_large());
    }
    if metadata.len() == 0 && name != Bundle::SUMMARY {
        return Err(io::Error::other("file is empty"));
    }
    Ok(file)
}

fn bundle_path(directory: &Path, name: &str) -> PathBuf {
    directory.join(name.split('/').collect::<PathBuf>())
}

pub(crate) fn read_file(directory: &Path, name: &str, limit: u64) -> io::Result<Vec<u8>> {
    read_from(&open_directory(directory)?, name, limit)
}

pub(in crate::show) fn read_from(
    directory: &ReadDirectory,
    name: &str,
    limit: u64,
) -> io::Result<Vec<u8>> {
    read_bounded(open_file(directory, name, limit)?, limit)
}

pub(super) fn read_bounded(file: File, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if !u64::try_from(bytes.len()).is_ok_and(|length| length <= limit) {
        return Err(too_large());
    }
    Ok(bytes)
}

fn too_large() -> io::Error {
    io::Error::new(io::ErrorKind::FileTooLarge, "file is too large")
}

pub(crate) fn summary(directory: &Path) -> io::Result<String> {
    summary_from(&open_directory(directory)?)
}

pub(in crate::show) fn summary_from(directory: &ReadDirectory) -> io::Result<String> {
    let bytes = read_from(directory, Bundle::SUMMARY, SUMMARY_LIMIT)?;
    String::from_utf8(bytes)
        .map_err(|_private_error| io::Error::other("summary.md is not valid UTF-8 text"))
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl SavedShow {
    pub(crate) fn folder(&self) -> anyhow::Result<Option<&Path>> {
        let metadata = found(fs::metadata(&self.0)).map_err(cannot_read(&self.0))?;
        Ok(metadata
            .is_some_and(|metadata| metadata.is_dir())
            .then_some(self.directory()))
    }

    pub(crate) fn summary(&self) -> anyhow::Result<Option<String>> {
        let path = self.0.join(Bundle::SUMMARY);
        match found(summary(&self.0)) {
            Err(error) if error.kind() == io::ErrorKind::FileTooLarge => Err(anyhow::anyhow!(
                "{} is too large to copy; a Show's summary is a few kilobytes.",
                path.display()
            )),
            read => read.map_err(cannot_read(&path)),
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
fn cannot_read(path: &Path) -> impl FnOnce(io::Error) -> anyhow::Error {
    move |error| anyhow::anyhow!("Cannot read {}: {error}.", path.display())
}

#[cfg(test)]
mod tests;
