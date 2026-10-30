//! Private Show checkpoints. A value owns the cross-process lock; drop only unlocks, leaving an
//! interrupted take. Explicit discard is durable before deletion and runs off the session owner.

mod audio;
mod keeper;

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Context as _;
pub(crate) use audio::{KeptAudio, MAX_CUES};
pub(crate) use keeper::{AudioLog, FrameLog, Keeper, Kept};
use speakeasy_show::{Log, Narration, Take};

use super::{Staged, age, recall};

const ROOT: &str = ".checkpoints";
const LOG: &str = "take.log";
const LOCK: &str = "owner.lock";
const DISCARDED: &str = "discarded";
const PUBLISHED: &str = "published";
const PROVENANCE: &str = ".checkpoint";
const NARRATION: &str = "narration.json";
const NARRATION_LIMIT: u64 = 4 << 20;
const GONE: &str = "This interrupted Show expired, was discarded, or was already recovered. \
                   Refresh Shows.";
const NO_FRAMES: &str = "No complete frame survived. Discard this interrupted Show.";
/// Counts all bytes, including metadata and obsolete versions that thinning no longer retains.
const FILE_LIMIT: u64 = 1 << 30;
const EVENT_LIMIT: usize = 200_000;

pub(crate) struct Checkpoint {
    directory: PathBuf,
    lock: OwnerLock,
}

/// Closing our descriptor alone can leave an inherited descriptor holding the same OS lock while
/// another thread spawns a process. Explicit unlock makes checkpoint ownership end with this value.
struct OwnerLock(File);

impl Drop for OwnerLock {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Drop cannot report an unlock failure; closing the owned descriptor remains the fallback"
        )]
        let _ = self.0.unlock();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Interrupted {
    directory: PathBuf,
    recorded: SystemTime,
    pending: bool,
}

impl Interrupted {
    pub(crate) const fn narration_pending(&self) -> bool {
        self.pending
    }

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
    fn create(parent: &Path, recorded: SystemTime) -> anyhow::Result<Self> {
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
        let directory = loop {
            let candidate = super::fresh_directory(&root, recorded)?;
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
                Err(error) => return Err(error.into()),
            }
        };
        let result = (|| {
            let lock = File::create_new(directory.join(LOCK))?;
            lock.lock()?;
            sync_directory(&directory)?;
            sync_directory(&root)?;
            anyhow::Ok(lock)
        })();
        match result {
            Ok(lock) => Ok(Self {
                directory,
                lock: OwnerLock(lock),
            }),
            Err(error) => {
                fs::remove_dir_all(directory)
                    .context("Cannot remove an unopened Show checkpoint")?;
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
            pending: self.pending()?,
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

    /// Stores recognized narration before erasing its audio. A crash between these steps retries
    /// deletion after reading the durable narration; it never needs a second transcription.
    pub(crate) fn narrate(&self, narration: &Narration, incomplete: bool) -> anyhow::Result<()> {
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(&StoredNarration {
                version: 1,
                narration: narration.clone(),
                incomplete,
            })
            .map_err(|_private_content| anyhow::anyhow!("Cannot encode recovered narration"))?,
        );
        anyhow::ensure!(
            u64::try_from(bytes.len())? <= NARRATION_LIMIT,
            "Show narration exceeds its storage limit"
        );
        let mut staged = tempfile::NamedTempFile::new_in(&self.directory)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        staged
            .persist(self.directory.join(NARRATION))
            .map_err(|error| error.error)?;
        sync_directory(&self.directory)?;
        // An older frame-only bundle cannot prove that it contains this new narration.
        // Retire its publication marker before erasing audio, so a crash or failed copy remains
        // recoverable from narration.json until a new bundle commits.
        remove(&self.directory.join(PUBLISHED))?;
        sync_directory(&self.directory)?;
        remove(&self.directory.join(audio::FILE))?;
        sync_directory(&self.directory)?;
        Ok(())
    }

    pub(crate) fn narration(&self) -> anyhow::Result<Option<(Narration, bool)>> {
        let bytes = match recall::read_file(&self.directory, NARRATION, NARRATION_LIMIT) {
            Ok(bytes) => zeroize::Zeroizing::new(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let saved: StoredNarration =
            serde_json::from_slice(&bytes).map_err(|_private_content| {
                anyhow::anyhow!("The saved Show narration is damaged or unsupported")
            })?;
        anyhow::ensure!(
            saved.version == 1,
            "The saved Show narration is unsupported"
        );
        Ok(Some((saved.narration, saved.incomplete)))
    }

    pub(crate) fn audio(&self) -> anyhow::Result<Option<KeptAudio>> {
        if !self.has_audio()? {
            return Ok(None);
        }
        recall::regular_file(&self.directory, audio::FILE, audio::FILE_LIMIT)?;
        audio::read(File::open(self.directory.join(audio::FILE))?).map(Some)
    }

    pub(crate) fn has_audio(&self) -> anyhow::Result<bool> {
        match fs::symlink_metadata(self.directory.join(audio::FILE)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub(in crate::show) fn pending(&self) -> anyhow::Result<bool> {
        let parent = self
            .directory
            .parent()
            .and_then(Path::parent)
            .context("Checkpoint has no parent")?;
        let id = self
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            .context("Checkpoint has no ID")?;
        Ok(published(parent, &self.directory, id)? && self.has_audio()?)
    }

    pub(crate) fn replay(&self) -> anyhow::Result<Take> {
        let mut log = recall::regular_file(&self.directory, LOG, FILE_LIMIT)
            .and_then(|()| File::open(self.directory.join(LOG)))
            .map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    anyhow::anyhow!(NO_FRAMES)
                } else {
                    error.into()
                }
            })?;
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
        anyhow::ensure!(!take.journal().is_empty(), NO_FRAMES);
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
        // Private content goes before the lock, so a blocked deletion remains retryable.
        for name in [audio::FILE, NARRATION, LOG] {
            remove(&self.directory.join(name))
                .context("Cannot remove Show content; close files open in the Show's folder")?;
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
        || (published(parent, directory, name)? && !checkpoint.has_audio()?))
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
            lock: OwnerLock(file),
        })),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredNarration {
    version: u32,
    narration: Narration,
    incomplete: bool,
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The session owns a live writer until both capture producers finish, then owns its files.
pub(crate) enum Checkpointing {
    Writing(Keeper),
    Held(Checkpoint),
}

impl Checkpointing {
    pub(crate) async fn close(self) -> anyhow::Result<Checkpoint> {
        match self {
            Self::Writing(keeper) => keeper.close().await,
            Self::Held(checkpoint) => Ok(checkpoint),
        }
    }

    pub(crate) async fn release(
        self,
        keep: bool,
        narration: Option<(Narration, bool)>,
    ) -> anyhow::Result<()> {
        let checkpoint = self.close().await?;
        tokio::task::spawn_blocking(move || {
            if !keep {
                return checkpoint.discard();
            }
            if let Some((narration, incomplete)) = narration {
                checkpoint.narrate(&narration, incomplete)?;
            }
            anyhow::Ok(())
        })
        .await
        .unwrap_or_else(|_private_panic| {
            Err(anyhow::anyhow!(
                "Show cleanup stopped. Check interrupted Shows."
            ))
        })
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

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Callers share Unix's fallible contract; portable Rust cannot sync a Windows directory handle"
)]
const fn sync_directory(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
