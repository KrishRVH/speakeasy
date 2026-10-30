//! Private Show checkpoints. A value owns the cross-process lock; drop only unlocks, leaving an
//! interrupted take. Explicit discard is durable before deletion and runs off the session owner.

mod writer;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Context as _;
use speakeasy_show::{Log, Size, Take};
pub(crate) use writer::Writer;

use super::{Staged, age, recall};

const ROOT: &str = ".checkpoints";
const LOG: &str = "take.log";
const LOCK: &str = "owner.lock";
const DISCARDED: &str = "discarded";
const PUBLISHED: &str = "published";
const PROVENANCE: &str = ".checkpoint";
const GONE: &str = "This interrupted Show expired, was discarded, or was already recovered. \
                   Refresh Shows.";
/// Counts all bytes, including metadata and obsolete versions that thinning no longer retains.
const FILE_LIMIT: u64 = 1 << 30;
const EVENT_LIMIT: usize = 200_000;

pub(crate) struct Checkpoint {
    directory: PathBuf,
    lock: File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Interrupted {
    directory: PathBuf,
    recorded: SystemTime,
}

impl Interrupted {
    pub(crate) fn id(&self) -> String {
        format!(
            "interrupted:{}",
            self.directory
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
    }

    pub(crate) fn expires_at(&self) -> Option<String> {
        let name = self.directory.file_name()?.to_str()?;
        super::retention::Retention::of(name, false).expires_at()
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) const fn recorded(&self) -> SystemTime {
        self.recorded
    }

    pub(crate) fn lock(&self) -> anyhow::Result<Checkpoint> {
        let held = lock(&self.directory).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                anyhow::anyhow!(GONE)
            } else {
                error.into()
            }
        })?;
        let checkpoint = held.context(
            "This Show is already recording, recovering, or being discarded. Refresh Shows.",
        )?;
        let parent = self
            .directory
            .parent()
            .and_then(Path::parent)
            .context("Checkpoint has no Shows folder")?;
        anyhow::ensure!(!retired(parent, &checkpoint, SystemTime::now())?, GONE);
        Ok(checkpoint)
    }
}

impl Checkpoint {
    /// Called only by the recording thread after an explicit Show acquired its capture target.
    pub(crate) fn begin(
        parent: &Path,
        recorded: SystemTime,
        budget: usize,
        surface: Size,
    ) -> anyhow::Result<(Self, Writer)> {
        let header = Log::header(budget, surface)?;
        let root = parent.join(ROOT);
        #[cfg(unix)]
        super::make_private(&root)?;
        #[cfg(not(unix))]
        fs::create_dir_all(&root)?;
        let metadata = fs::symlink_metadata(&root)?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Show checkpoints must be a directory, not a link"
        );
        let directory = super::fresh_directory(&root, recorded)?;
        fs::create_dir(&directory)?;
        let file = File::create_new(directory.join(LOCK))?;
        file.lock()?;
        let checkpoint = Self {
            directory,
            lock: file,
        };
        let result = (|| {
            let mut log = File::create_new(checkpoint.directory.join(LOG))?;
            log.write_all(&header)?;
            log.sync_all()?;
            sync_directory(&checkpoint.directory)?;
            sync_directory(&root)?;
            Writer::start(log)
        })();
        match result {
            Ok(writer) => Ok((checkpoint, writer)),
            Err(error) => {
                checkpoint
                    .discard()
                    .context("Could not remove a checkpoint that failed to start")?;
                Err(error)
            },
        }
    }

    pub(crate) fn recorded(&self) -> anyhow::Result<SystemTime> {
        self.directory
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(age::recorded_at)
            .context("Checkpoint recording date is invalid")
    }

    /// Releases ownership and describes the interrupted take without reading its private log.
    pub(crate) fn interrupted(self) -> anyhow::Result<Interrupted> {
        Ok(Interrupted {
            recorded: self.recorded()?,
            directory: self.directory,
        })
    }

    /// A publication's two markers bind its final folder to this checkpoint. A crash between the
    /// markers and rename can be retried; a coincidental folder name never proves publication.
    pub(in crate::show) fn commit(&self, staged: &Staged) -> anyhow::Result<()> {
        let id = self
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            .context("Checkpoint has no ID")?;
        let target = staged
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            .context("Show has no ID")?;
        super::write_flushed(&staged.staging.path().join(PROVENANCE), id.as_bytes())?;
        sync_directory(staged.staging.path())?;
        let mut marker = tempfile::NamedTempFile::new_in(&self.directory)?;
        marker.write_all(target.as_bytes())?;
        marker.as_file().sync_all()?;
        marker
            .persist(self.directory.join(PUBLISHED))
            .map_err(|error| error.error)?;
        sync_directory(&self.directory)?;
        Ok(())
    }

    pub(crate) fn replay(&self) -> anyhow::Result<Take> {
        recall::regular_file(&self.directory, LOG, FILE_LIMIT)?;
        let mut log = File::open(self.directory.join(LOG))?;
        let mut header = [0; Log::HEADER_BYTES];
        log.read_exact(&mut header)
            .context("The Show checkpoint header is incomplete")?;
        let mut take = Log::start(&header)?;
        let mut consumed = u64::try_from(Log::HEADER_BYTES)?;
        for _ in 0..EVENT_LIMIT {
            let mut length = [0; 4];
            if !read_complete(&mut log, &mut length)? {
                break;
            }
            let Ok(size) = Log::record_size(&length) else {
                break;
            };
            consumed = consumed.saturating_add(u64::try_from(size)?);
            if consumed > FILE_LIMIT {
                break;
            }
            let mut record = vec![0; size];
            let Some((prefix, rest)) = record.split_at_mut_checked(4) else {
                break;
            };
            prefix.copy_from_slice(&length);
            if !read_complete(&mut log, rest)? {
                break;
            }
            let Ok(event) = Log::decode(&record) else {
                break;
            };
            if take.apply(&event).is_err() {
                break;
            }
            if take.ended() {
                break;
            }
        }
        anyhow::ensure!(
            !take.journal().is_empty(),
            "No complete frame survived. Discard this interrupted Show."
        );
        Ok(take)
    }

    /// Marker creation precedes unlock and deletion. If deletion fails, cleanup retries it without
    /// offering the cancelled Show for recovery. No destructor performs this I/O.
    pub(crate) fn discard(self) -> anyhow::Result<()> {
        match File::create_new(self.directory.join(DISCARDED)) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                regular(&self.directory.join(DISCARDED))?;
                OpenOptions::new()
                    .write(true)
                    .open(self.directory.join(DISCARDED))?
                    .sync_all()?;
            },
            Err(error) => return Err(error.into()),
        }
        sync_directory(&self.directory)?;
        // Remove private frames before the lock. If a file is held open, the durable discard
        // marker and lock survive so the next sweep can retry instead of orphaning the log.
        match fs::remove_file(self.directory.join(LOG)) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => {
                return Err(error).context(
                    "Cannot remove recorded frames; close files open in the Show's folder",
                );
            },
        }
        sync_directory(&self.directory)?;
        let Self { directory, lock } = self;
        drop(lock);
        fs::remove_dir_all(&directory)
            .context("Cannot remove the discarded Show; close files open in its folder")?;
        if let Some(parent) = directory.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }
}

/// Listing does not change files, permissions, or expiry. Active and retired checkpoints are
/// omitted; the retention sweep owns their deletion.
pub(crate) struct Listing {
    pub shows: Vec<Interrupted>,
    pub problems: Vec<Problem>,
}

pub(crate) struct Problem {
    pub directory: PathBuf,
    pub message: String,
}

pub(crate) fn list(parent: &Path, now: SystemTime) -> Listing {
    let mut shows = Vec::new();
    let problems = visit(parent, now, |checkpoint, retired| {
        if !retired {
            shows.push(checkpoint.interrupted()?);
        }
        Ok(())
    });
    shows.sort_by(|a, b| b.directory.cmp(&a.directory));
    Listing { shows, problems }
}

/// Only explicit interrupted IDs or exact managed paths select checkpoints; saved IDs never alias.
pub(crate) fn resolve(parent: &Path, target: &Path) -> anyhow::Result<Interrupted> {
    let path = std::path::absolute(target)?;
    list(parent, SystemTime::now())
        .shows
        .into_iter()
        .find(|show| target.to_str().is_some_and(|id| id == show.id()) || path == show.directory)
        .context("No interrupted Show matches that ID or path. Run speakeasy show list --json.")
}

pub(super) fn sweep(parent: &Path, now: SystemTime) -> anyhow::Result<()> {
    let problems = visit(parent, now, |checkpoint, retired| {
        if retired {
            checkpoint.discard()
        } else {
            Ok(())
        }
    });
    if let Some(problem) = problems.first() {
        anyhow::bail!(
            "Cannot clean up every interrupted Show: {} ({}). Check folder access and close open files; cleanup retries within the hour.",
            problem.message,
            problem.directory.display()
        );
    }
    Ok(())
}

/// One damaged entry cannot prevent discovery or cleanup of its siblings. Root errors are also
/// returned as findings, so callers can still inspect and expire saved bundles.
fn visit(
    parent: &Path,
    now: SystemTime,
    mut each: impl FnMut(Checkpoint, bool) -> anyhow::Result<()>,
) -> Vec<Problem> {
    let root = parent.join(ROOT);
    let mut problems = Vec::new();
    let result = (|| {
        match fs::symlink_metadata(&root) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Show checkpoints must be a directory, not a link"
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        for entry in fs::read_dir(&root)? {
            let directory = entry
                .as_ref()
                .map_or_else(|_| root.clone(), std::fs::DirEntry::path);
            let checked = (|| {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    return Ok(());
                }
                let name = entry.file_name();
                if name.to_str().and_then(age::expires_at).is_none() {
                    return Ok(());
                }
                let checkpoint = match lock(&directory) {
                    Ok(Some(checkpoint)) => checkpoint,
                    Ok(None) => return Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error.into()),
                };
                let is_retired = retired(parent, &checkpoint, now)?;
                each(checkpoint, is_retired)
            })();
            if let Err(error) = checked {
                problems.push(Problem {
                    directory,
                    message: format!("{error:#}"),
                });
            }
        }
        anyhow::Ok(())
    })();
    if let Err(error) = result {
        problems.push(Problem {
            directory: root,
            message: format!("{error:#}"),
        });
    }
    problems
}

fn retired(parent: &Path, checkpoint: &Checkpoint, now: SystemTime) -> anyhow::Result<bool> {
    let directory = &checkpoint.directory;
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .context("Checkpoint has no ID")?;
    let discarded = match fs::symlink_metadata(directory.join(DISCARDED)) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    Ok(discarded
        || age::expires_at(name).is_none_or(|expiry| expiry <= now)
        || published(parent, directory, name)?)
}

fn published(parent: &Path, directory: &Path, id: &str) -> anyhow::Result<bool> {
    let marker = match recall::read_file(directory, PUBLISHED, 128) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let target = std::str::from_utf8(&marker)
        .ok()
        .filter(|name| age::recorded_at(name).is_some())
        .context("Checkpoint publication marker is invalid")?;
    let target = parent.join(target);
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    match recall::read_file(&target, PROVENANCE, 128) {
        Ok(bytes) => Ok(bytes == id.as_bytes()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn lock(directory: &Path) -> io::Result<Option<Checkpoint>> {
    let parent = directory
        .parent()
        .ok_or_else(|| io::Error::other("Checkpoint has no parent"))?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other(
            "Checkpoint parent must be a directory, not a link",
        ));
    }
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other(
            "Checkpoint must be a directory, not a link",
        ));
    }
    regular(&directory.join(LOCK))?;
    let file = File::open(directory.join(LOCK))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Checkpoint {
            directory: directory.to_owned(),
            lock: file,
        })),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

fn regular(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(io::Error::other(
            "Checkpoint metadata must be a regular file",
        ))
    }
}

fn read_complete(file: &mut File, bytes: &mut [u8]) -> io::Result<bool> {
    match file.read_exact(bytes) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _directory = directory;
    Ok(())
}

#[cfg(test)]
mod tests;
