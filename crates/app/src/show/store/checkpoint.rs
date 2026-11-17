pub(in crate::show::store) mod audio;
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
use speakeasy_show::{Bundle, Moment, Narration, Take, log};

use super::{
    Staged, address::ShowId, age, entry_exists, found, movie, recall, remove_if_present,
    sync_directory,
};

const ROOT: &str = ".checkpoints";
const LOG: &str = "take.log";
const LOCK: &str = "owner.lock";
const DISCARDED: &str = "discarded";
const PUBLISHED: &str = "published";
const NARRATED: &str = "\nnarrated";
const MARKER_LIMIT: u64 = 128;
const PROVENANCE: &str = ".checkpoint";
const NARRATION: &str = "narration.json";
const NARRATION_VERSION: u32 = 1;
const NARRATION_LIMIT: u64 = 4 << 20;
const GONE: &str = "This interrupted Show expired, was discarded, or was already recovered. \
                   Refresh Shows.";
const NO_FRAMES: &str = "No complete frame survived. Discard this interrupted Show.";
const ROOT_LINKED: &str = "Show checkpoints must be a directory, not a link";

const FILE_LIMIT: u64 = 1 << 30;
const EVENT_LIMIT: usize = 200_000;

pub(crate) struct Checkpoint {
    directory: PathBuf,
    lock: OwnerLock,
}

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
        let held = found(lock(&self.directory))?.context(GONE)?;
        let checkpoint = held.context(
            "This Show is already recording, recovering, or being discarded. Refresh Shows.",
        )?;
        anyhow::ensure!(!checkpoint.retired(SystemTime::now())?, GONE);
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
        anyhow::ensure!(fs::symlink_metadata(&root)?.is_dir(), ROOT_LINKED);
        let directory = loop {
            let candidate = super::fresh_directory(&root, recorded)?;
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
                Err(error) => return Err(error.into()),
            }
        };
        let locked = File::create_new(directory.join(LOCK)).and_then(|lock| {
            lock.lock()?;
            sync_directory(&directory)?;
            sync_directory(&root)?;
            Ok(lock)
        });
        match locked {
            Ok(lock) => Ok(Self {
                directory,
                lock: OwnerLock(lock),
            }),
            Err(error) => {
                fs::remove_dir_all(directory)
                    .context("Cannot remove an unopened Show checkpoint")?;
                Err(error.into())
            },
        }
    }

    fn id(&self) -> anyhow::Result<&str> {
        self.directory
            .file_name()
            .and_then(|name| name.to_str())
            .context("Checkpoint has no ID")
    }

    pub(crate) fn recorded(&self) -> anyhow::Result<SystemTime> {
        age::recorded_at(self.id()?).context("Checkpoint recording date is invalid")
    }

    pub(crate) fn interrupted(self) -> anyhow::Result<Interrupted> {
        Ok(Interrupted {
            recorded: self.recorded()?,
            pending: self.pending()?,
            directory: self.directory,
        })
    }

    pub(in crate::show) fn commit(
        &self,
        staged: &Staged,
        publication: Publication,
    ) -> anyhow::Result<()> {
        let id = self.id()?;
        let target = staged
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            .context("Show has no ID")?;
        super::write_flushed(&staged.staging.path().join(PROVENANCE), id.as_bytes())?;
        sync_directory(staged.staging.path())?;
        let mut marker = tempfile::NamedTempFile::new_in(&self.directory)?;
        marker.write_all(target.as_bytes())?;
        if publication == Publication::Narrated {
            marker.write_all(NARRATED.as_bytes())?;
        }
        marker.as_file().sync_all()?;
        marker
            .persist(self.directory.join(PUBLISHED))
            .map_err(|error| error.error)?;
        sync_directory(&self.directory)?;
        Ok(())
    }

    pub(in crate::show) fn retain_movie(
        &self,
        staged: &mut Staged,
        annotations: Moment,
    ) -> anyhow::Result<bool> {
        let lost = match movie::checkpoint(&self.directory)? {
            movie::CheckpointMovie::Absent => return Ok(false),
            movie::CheckpointMovie::Unavailable => {
                staged.summary.push_str("\n\nVideo could not be saved: no complete fragment survived. The saved keyframes and narration remain available.\n");
                true
            },
            movie::CheckpointMovie::Present(movie) => {
                let metadata = movie::retain(staged.staging.path(), *movie, annotations).context(
                    "Cannot retain the recorded movie; its checkpoint remains available",
                )?;
                staged
                    .summary
                    .push_str(&metadata.note(&staged.directory.join(movie::FILE))?);
                false
            },
        };
        let mut summary = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(staged.staging.path().join(Bundle::SUMMARY))?;
        summary.write_all(staged.summary.as_bytes())?;
        summary.sync_all()?;
        sync_directory(staged.staging.path())?;
        Ok(lost)
    }

    pub(in crate::show) fn retain_voice(&self, staged: &Staged) -> anyhow::Result<()> {
        super::voice::retain(&self.directory, staged, self.recorded()?)
    }

    pub(crate) fn narrate(&self, narration: &Narration, incomplete: bool) -> anyhow::Result<()> {
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(&StoredNarration {
                version: NARRATION_VERSION,
                narration,
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

        remove_if_present(&self.directory.join(PUBLISHED))?;
        sync_directory(&self.directory)?;
        Ok(())
    }

    pub(crate) fn narration(&self) -> anyhow::Result<Option<(Narration, bool)>> {
        let read = recall::read_file(&self.directory, NARRATION, NARRATION_LIMIT)
            .map(zeroize::Zeroizing::new);
        let Some(bytes) = found(read)? else {
            return Ok(None);
        };
        let saved: StoredNarration<Narration> =
            serde_json::from_slice(&bytes).map_err(|_private_content| {
                anyhow::anyhow!("The saved Show narration is damaged or unsupported")
            })?;
        anyhow::ensure!(
            saved.version == NARRATION_VERSION,
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
        Ok(entry_exists(&self.directory.join(audio::FILE))?)
    }

    pub(in crate::show) fn pending(&self) -> anyhow::Result<bool> {
        Ok(self.publication()? == Some(Publication::Pending) && self.has_audio()?)
    }

    fn retired(&self, now: SystemTime) -> anyhow::Result<bool> {
        let discarded = entry_exists(&self.directory.join(DISCARDED))?;
        Ok(discarded
            || age::expires_at(self.id()?).is_none_or(|expiry| expiry <= now)
            || match self.publication()? {
                Some(Publication::Narrated) => true,
                Some(Publication::Pending) => !self.has_audio()?,
                None => false,
            })
    }

    fn publication(&self) -> anyhow::Result<Option<Publication>> {
        let Some(marker) = found(recall::read_file(&self.directory, PUBLISHED, MARKER_LIMIT))?
        else {
            return Ok(None);
        };
        let (target, publication) = published(&marker)
            .filter(|(target, _)| age::recorded_at(target).is_some())
            .context("Checkpoint publication marker is invalid")?;
        let target = self
            .directory
            .parent()
            .and_then(Path::parent)
            .context("Checkpoint has no Shows folder")?
            .join(target);
        if !found(fs::symlink_metadata(&target))?.is_some_and(|metadata| metadata.is_dir()) {
            return Ok(None);
        }
        let Some(provenance) = found(recall::read_file(&target, PROVENANCE, MARKER_LIMIT))? else {
            return Ok(None);
        };
        Ok((provenance == self.id()?.as_bytes()).then_some(publication))
    }

    pub(crate) fn replay(&self) -> anyhow::Result<Take> {
        let opened = recall::regular_file(&self.directory, LOG, FILE_LIMIT)
            .and_then(|()| File::open(self.directory.join(LOG)));
        let mut reader = io::BufReader::new(found(opened)?.context(NO_FRAMES)?);
        let mut header = [0; log::HEADER_BYTES];
        reader
            .read_exact(&mut header)
            .context("The Show checkpoint header is incomplete")?;
        let mut take = log::start(&header)?;
        let mut consumed = u64::try_from(log::HEADER_BYTES)?;
        for _ in 0..EVENT_LIMIT {
            let mut length = [0; 4];
            if !read_complete(&mut reader, &mut length)? {
                break;
            }
            let Ok(size) = log::record_size(&length) else {
                break;
            };
            consumed = consumed.saturating_add(u64::try_from(size)?);
            if consumed > FILE_LIMIT {
                break;
            }
            let mut record = vec![0; size];
            let Some((prefix, rest)) = record.split_first_chunk_mut::<4>() else {
                break;
            };
            *prefix = length;
            if !read_complete(&mut reader, rest)? {
                break;
            }
            let Ok(event) = log::decode(&record) else {
                break;
            };
            if take.apply(&event).is_err() || take.ended() {
                break;
            }
        }
        anyhow::ensure!(!take.journal().is_empty(), NO_FRAMES);
        Ok(take)
    }

    pub(crate) fn discard(self) -> anyhow::Result<()> {
        let discarded = self.directory.join(DISCARDED);
        match File::create_new(&discarded) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                regular(&discarded)?;
                OpenOptions::new()
                    .write(true)
                    .open(&discarded)?
                    .sync_all()?;
            },
            Err(error) => return Err(error.into()),
        }
        sync_directory(&self.directory)?;

        for name in [
            audio::FILE,
            NARRATION,
            LOG,
            movie::FILE,
            movie::HEADER,
            movie::geometry::FILE,
        ] {
            remove_if_present(&self.directory.join(name))
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

pub(crate) struct Listing {
    pub shows: Vec<Interrupted>,
    pub problems: Vec<Problem>,
}

pub(super) fn discard_published(
    parent: &Path,
    directory: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    const DAMAGED: &str = "Cannot verify this Show's recovery link. Check its .checkpoint and recovery files before deleting it.";
    let Some(bytes) =
        found(recall::read_file(directory, PROVENANCE, MARKER_LIMIT)).context(DAMAGED)?
    else {
        return Ok(None);
    };
    let id = std::str::from_utf8(&bytes)
        .ok()
        .and_then(ShowId::parse)
        .context(DAMAGED)?;
    let path = parent.join(ROOT).join(id.as_str());
    if !entry_exists(&path).context(DAMAGED)? {
        return Ok(None);
    }
    let checkpoint = lock(&path).context(DAMAGED)?.context(
        "This Show is still recording or recovering. Let it finish, then try deleting it again.",
    )?;
    let marker = recall::read_file(&path, PUBLISHED, MARKER_LIMIT).context(DAMAGED)?;
    let target = published(&marker)
        .and_then(|(target, _)| ShowId::parse(target))
        .context(DAMAGED)?;
    if directory.file_name().and_then(|name| name.to_str()) != Some(target.as_str()) {
        return Ok(None);
    }
    checkpoint.discard()?;
    Ok(Some(path))
}

pub(crate) struct Problem {
    pub directory: PathBuf,
    pub message: String,
}

impl Problem {
    fn new(directory: PathBuf, error: &anyhow::Error) -> Self {
        Self {
            directory,
            message: format!("{error:#}"),
        }
    }
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

fn visit(
    parent: &Path,
    now: SystemTime,
    mut each: impl FnMut(Checkpoint, bool) -> anyhow::Result<()>,
) -> Vec<Problem> {
    let root = parent.join(ROOT);
    let entries = match entries(&root) {
        Ok(Some(entries)) => entries,
        Ok(None) => return Vec::new(),
        Err(error) => return vec![Problem::new(root, &error)],
    };
    entries
        .filter_map(|entry| match entry {
            Ok(entry) => visit_entry(&entry, now, &mut each)
                .err()
                .map(|error| Problem::new(entry.path(), &error)),
            Err(error) => Some(Problem::new(root.clone(), &error.into())),
        })
        .collect()
}

fn entries(root: &Path) -> anyhow::Result<Option<fs::ReadDir>> {
    let Some(metadata) = found(fs::symlink_metadata(root))? else {
        return Ok(None);
    };
    anyhow::ensure!(metadata.is_dir(), ROOT_LINKED);
    Ok(Some(fs::read_dir(root)?))
}

fn visit_entry(
    entry: &fs::DirEntry,
    now: SystemTime,
    each: &mut impl FnMut(Checkpoint, bool) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if !entry.file_type()?.is_dir()
        || entry
            .file_name()
            .to_str()
            .and_then(age::expires_at)
            .is_none()
    {
        return Ok(());
    }
    let Some(checkpoint) = found(lock(&entry.path()))?.flatten() else {
        return Ok(());
    };
    let retired = checkpoint.retired(now)?;
    each(checkpoint, retired)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::show) enum Publication {
    Pending,
    Narrated,
}

fn published(marker: &[u8]) -> Option<(&str, Publication)> {
    let marker = std::str::from_utf8(marker).ok()?;
    Some(match marker.strip_suffix(NARRATED) {
        Some(target) => (target, Publication::Narrated),
        None => (marker, Publication::Pending),
    })
}

fn lock(directory: &Path) -> io::Result<Option<Checkpoint>> {
    let parent = directory
        .parent()
        .ok_or_else(|| io::Error::other("Checkpoint has no parent"))?;
    if !fs::symlink_metadata(parent)?.is_dir() {
        return Err(io::Error::other(
            "Checkpoint parent must be a directory, not a link",
        ));
    }
    if !fs::symlink_metadata(directory)?.is_dir() {
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
struct StoredNarration<N> {
    version: u32,
    narration: N,
    incomplete: bool,
}

pub(crate) enum Checkpointing {
    Writing(Keeper),
    Held(Checkpoint),
}

pub(crate) enum Release {
    Discard,
    Keep(Option<(Narration, bool)>),
}

impl Checkpointing {
    pub(crate) async fn close(self) -> anyhow::Result<Option<Checkpoint>> {
        match self {
            Self::Writing(keeper) => keeper.close().await,
            Self::Held(checkpoint) => Ok(Some(checkpoint)),
        }
    }

    pub(crate) async fn release(self, release: Release) -> anyhow::Result<()> {
        let Some(checkpoint) = self.close().await? else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || match release {
            Release::Discard => checkpoint.discard(),
            Release::Keep(Some((narration, incomplete))) => {
                checkpoint.narrate(&narration, incomplete)
            },
            Release::Keep(None) => Ok(()),
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
    if fs::symlink_metadata(path)?.is_file() {
        Ok(())
    } else {
        Err(io::Error::other(
            "Checkpoint metadata must be a regular file",
        ))
    }
}

fn read_complete(reader: &mut impl io::Read, bytes: &mut [u8]) -> io::Result<bool> {
    match reader.read_exact(bytes) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
