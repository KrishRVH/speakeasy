use std::{
    fmt, fs,
    io::{self, ErrorKind, Read as _},
    path::{Path, PathBuf},
};

use serde::Serialize;
use speakeasy_show::{Bundle, Manifest, Unreadable};

#[cfg(any(target_os = "windows", target_os = "macos", test))]
use super::SavedShow;
use super::{PIN, pinned, stamp};

pub(super) const SUMMARY_LIMIT: u64 = 1 << 20;

pub(super) const MANIFEST_LIMIT: u64 = 16 << 20;

pub(crate) const IMAGE_LIMIT: u64 = 32 << 20;

#[derive(Debug)]
pub(crate) struct ReadShow {
    pub manifest: Manifest,
    pub pinned: bool,
    pub problems: Vec<FileProblem>,
    pub movie: Option<super::movie::Metadata>,
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

    pub(crate) fn open(directory: &Path) -> io::Result<Self> {
        let directory = std::path::absolute(directory)?;
        if !fs::metadata(&directory)?.is_dir() {
            return Err(io::Error::other("a Show must be a directory"));
        }
        let show = read(&directory);
        Ok(Self { directory, show })
    }

    pub(crate) fn file(&self, name: &str) -> PathBuf {
        self.directory.join(name.split('/').collect::<PathBuf>())
    }
}

pub(crate) fn list(parent: &Path) -> io::Result<impl Iterator<Item = io::Result<Entry>>> {
    let paths = discover(parent)?;
    Ok(paths.into_iter().filter_map(|path| {
        let inspected = Entry::open(&path).and_then(|entry| {
            fs::metadata(&entry.directory)?;
            Ok(entry)
        });
        match inspected {
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            result => Some(result),
        }
    }))
}

fn discover(parent: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(parent) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        entries => entries?,
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some((name, stamp)) = name
            .to_str()
            .and_then(|name| stamp(name).map(|stamp| (name, stamp)))
        else {
            continue;
        };
        let kind = match entry.file_type() {
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            kind => kind?,
        };
        if !kind.is_dir() {
            continue;
        }
        let suffix = name
            .split_once('-')
            .map_or(1, |(_, number)| number.parse::<u64>().unwrap_or(u64::MAX));
        found.push((stamp.to_owned(), suffix, entry.path()));
    }
    found.sort_by(|a, b| b.cmp(a));
    Ok(found.into_iter().map(|(_, _, entry)| entry).collect())
}

fn read(directory: &Path) -> Result<ReadShow, ReadError> {
    let file_error = |name: &str, error: io::Error| {
        ReadError::File(FileProblem {
            name: name.to_owned(),
            reason: error.to_string(),
        })
    };
    let bytes = read_file(directory, Manifest::FILE, MANIFEST_LIMIT)
        .map_err(|error| file_error(Manifest::FILE, error))?;
    let manifest = Manifest::parse(&bytes).map_err(ReadError::Manifest)?;
    let pinned = pinned(directory).map_err(|error| file_error(PIN, error))?;
    let mut problems: Vec<_> = manifest
        .files()
        .filter_map(|name| {
            let limit = if name == Bundle::SUMMARY {
                SUMMARY_LIMIT
            } else {
                u64::MAX
            };
            regular_file(directory, &name, limit)
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

pub(super) fn regular_file(directory: &Path, name: &str, limit: u64) -> io::Result<()> {
    let mut path = directory.to_owned();
    let mut parts = name.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        let metadata = fs::symlink_metadata(&path)?;
        if parts.peek().is_some() {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(io::Error::other("parent must be a directory, not a link"));
            }
        } else {
            if !metadata.is_file() {
                return Err(io::Error::other("not a regular file"));
            }
            if metadata.len() > limit {
                return Err(io::Error::other("file is too large"));
            }
            if metadata.len() == 0 && name != Bundle::SUMMARY {
                return Err(io::Error::other("file is empty"));
            }
            return Ok(());
        }
    }
    Err(io::Error::other("missing file name"))
}

pub(crate) fn read_file(directory: &Path, name: &str, limit: u64) -> io::Result<Vec<u8>> {
    regular_file(directory, name, limit)?;
    let mut bytes = Vec::new();
    fs::File::open(directory.join(name.split('/').collect::<PathBuf>()))?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if !u64::try_from(bytes.len()).is_ok_and(|length| length <= limit) {
        return Err(io::Error::other("file is too large"));
    }
    Ok(bytes)
}

pub(crate) fn summary(directory: &Path) -> io::Result<String> {
    let bytes = read_file(directory, Bundle::SUMMARY, SUMMARY_LIMIT)?;
    String::from_utf8(bytes)
        .map_err(|_private_error| io::Error::other("summary.md is not valid UTF-8 text"))
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl SavedShow {
    pub(crate) fn folder(&self) -> anyhow::Result<Option<&Path>> {
        match fs::metadata(&self.0) {
            Ok(metadata) => Ok(metadata.is_dir().then_some(&*self.0)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(cannot_read(&self.0)(error)),
        }
    }

    pub(crate) fn summary(&self) -> anyhow::Result<Option<String>> {
        let path = self.0.join(Bundle::SUMMARY);
        let file = match fs::File::open(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            file => file.map_err(cannot_read(&path))?,
        };
        let mut summary = String::new();
        file.take(SUMMARY_LIMIT.saturating_add(1))
            .read_to_string(&mut summary)
            .map_err(cannot_read(&path))?;
        anyhow::ensure!(
            u64::try_from(summary.len()).is_ok_and(|length| length <= SUMMARY_LIMIT),
            "{} is too large to copy; a Show's summary is a few kilobytes.",
            path.display()
        );
        Ok(Some(summary))
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
fn cannot_read(path: &Path) -> impl FnOnce(io::Error) -> anyhow::Error {
    move |error| anyhow::anyhow!("Cannot read {}: {error}.", path.display())
}

#[cfg(test)]
mod tests;
