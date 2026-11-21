pub(in crate::show::store) mod audio;
mod keeper;

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    iter,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Context as _;
pub(in crate::show) use audio::Damaged;
pub(crate) use audio::{KeptAudio, MAX_CUES};
pub(crate) use keeper::{AudioEnding, AudioLog, AudioTail, FrameLog, Keeper, Kept};
use speakeasy_show::{Bundle, Moment, Narration, NarrationLoss, Take, log};

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
const SUPERSEDES: &str = "\nsupersedes ";
const MARKER_LIMIT: u64 = 128;
const PROVENANCE: &str = ".checkpoint";
const NARRATION: &str = "narration.json";
const NARRATION_VERSION: u32 = 1;
const NARRATION_LIMIT: u64 = 4 << 20;
const GONE: &str = "This interrupted Show expired, was discarded, or was already recovered. \
                   Refresh Shows.";
const NO_FRAMES: &str = "No complete frame survived. Discard this interrupted Show.";
const BUSY: &str = "This Show is already recording, recovering, or being discarded. Refresh Shows.";
const UNMATCHED: &str =
    "No interrupted Show matches that ID or path. Run speakeasy show list --json.";
const UNRETRYABLE: &str = "This Show's kept audio is damaged, so its narration cannot be retried. \
                           The saved Show is unaffected; remove this recovery folder with \
                           speakeasy show discard.";
const UNLANDED: &str = "Cannot confirm that the saved Show reached the disk, so its recovery \
                        files stay. Check access to the Shows folder; cleanup retries within \
                        the hour";
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
        label(&self.directory)
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
        let checkpoint = found(lock(&self.directory))?.context(GONE)?.context(BUSY)?;
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
        let superseded = self
            .association()?
            .filter(|(_, previous)| *previous == Publication::Pending);
        super::write_flushed(&staged.staging.path().join(PROVENANCE), id.as_bytes())?;
        sync_directory(staged.staging.path())?;
        let mut marker = tempfile::NamedTempFile::new_in(&self.directory)?;
        marker.write_all(target.as_bytes())?;
        if publication == Publication::Narrated {
            marker.write_all(NARRATED.as_bytes())?;
        }
        if let Some((previous, _)) = superseded {
            marker.write_all(SUPERSEDES.as_bytes())?;
            marker.write_all(previous.as_str().as_bytes())?;
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

    pub(in crate::show) fn retain_voice(&self, staged: &Staged) -> anyhow::Result<bool> {
        super::voice::retain(&self.directory, staged, self.recorded()?)
            .context("Cannot retain Show voice; its checkpoint remains available")
    }

    pub(crate) fn narrate(
        &self,
        narration: &Narration,
        loss: Option<NarrationLoss>,
    ) -> anyhow::Result<()> {
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(&StoredNarration {
                version: NARRATION_VERSION,
                narration,
                incomplete: loss == Some(NarrationLoss::Incomplete),
                audio_silent: loss == Some(NarrationLoss::AudioSilent),
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
        Ok(())
    }

    pub(crate) fn narration(&self) -> io::Result<SavedNarration> {
        let read = found(recall::read_file(
            &self.directory,
            NARRATION,
            NARRATION_LIMIT,
        ));
        let bytes = match read {
            Ok(Some(bytes)) => zeroize::Zeroizing::new(bytes),
            Ok(None) => return Ok(SavedNarration::Absent),
            Err(error) if error.kind() == io::ErrorKind::FileTooLarge || recall::empty(&error) => {
                return Ok(SavedNarration::Damaged);
            },
            Err(error) => return Err(error),
        };
        let Some(saved) = serde_json::from_slice::<StoredNarration<Narration>>(&bytes)
            .ok()
            .filter(|saved| saved.version == NARRATION_VERSION)
        else {
            return Ok(SavedNarration::Damaged);
        };
        let loss = if saved.audio_silent {
            Some(NarrationLoss::AudioSilent)
        } else {
            saved.incomplete.then_some(NarrationLoss::Incomplete)
        };
        Ok(SavedNarration::Saved(saved.narration, loss))
    }

    pub(crate) fn audio(&self) -> anyhow::Result<Option<KeptAudio>> {
        audio::available(&self.directory, audio::FILE)?
            .map(audio::read)
            .transpose()
    }

    pub(crate) fn has_audio(&self) -> anyhow::Result<bool> {
        Ok(audio::available(&self.directory, audio::FILE)?.is_some())
    }

    pub(in crate::show) fn pending(&self) -> anyhow::Result<bool> {
        if self.publication()? != Some(Publication::Pending) {
            return Ok(false);
        }
        self.retryable().map_err(|error| {
            if error.is::<audio::Damaged>() {
                anyhow::anyhow!(UNRETRYABLE)
            } else {
                error
            }
        })
    }

    fn retryable(&self) -> anyhow::Result<bool> {
        Ok(entry_exists(&self.directory.join(NARRATION))? || self.has_audio()?)
    }

    fn retired(&self, now: SystemTime) -> anyhow::Result<bool> {
        if entry_exists(&self.directory.join(DISCARDED))?
            || age::expires_at(self.id()?).is_none_or(|expiry| expiry <= now)
        {
            return Ok(true);
        }
        let superseded = match self.publication()? {
            Some(Publication::Narrated) => true,
            Some(Publication::Pending) => self.retryable().is_ok_and(|retryable| !retryable),
            None => false,
        };
        if superseded {
            self.landed()?;
        }
        Ok(superseded)
    }

    pub(in crate::show) fn retire(self) -> anyhow::Result<()> {
        self.landed()?;
        self.discard()
    }

    fn landed(&self) -> anyhow::Result<()> {
        sync_directory(self.shows()?).context(UNLANDED)
    }

    fn shows(&self) -> anyhow::Result<&Path> {
        self.directory
            .parent()
            .and_then(Path::parent)
            .context("Checkpoint has no Shows folder")
    }

    fn publication(&self) -> anyhow::Result<Option<Publication>> {
        Ok(self.association()?.map(|(_, publication)| publication))
    }

    fn association(&self) -> anyhow::Result<Option<(ShowId, Publication)>> {
        let Some(bytes) = found(recall::read_file(&self.directory, PUBLISHED, MARKER_LIMIT))?
        else {
            return Ok(None);
        };
        let marker = Marker::parse(&bytes).context("Checkpoint publication marker is invalid")?;
        self.linked(marker)
    }

    fn linked(&self, marker: Marker) -> anyhow::Result<Option<(ShowId, Publication)>> {
        let Marker {
            target,
            publication,
            superseded,
        } = marker;
        let id = self.id()?;
        let shows = self.shows()?;
        let candidates = iter::once((target, publication))
            .chain(superseded.map(|previous| (previous, Publication::Pending)));
        for (target, publication) in candidates {
            let bundle = shows.join(target.as_str());
            if found(fs::symlink_metadata(&bundle))?.is_some_and(|metadata| metadata.is_dir())
                && found(recall::read_file(&bundle, PROVENANCE, MARKER_LIMIT))?
                    .is_some_and(|provenance| provenance == id.as_bytes())
            {
                return Ok(Some((target, publication)));
            }
        }
        Ok(None)
    }

    pub(crate) fn replay(&self) -> anyhow::Result<Take> {
        self.read_take(Extent::Whole)
    }

    pub(in crate::show) fn keeps_retry(&self) -> bool {
        let audio = self
            .has_audio()
            .unwrap_or_else(|error| !error.is::<audio::Damaged>());
        audio
            && match self.read_take(Extent::FirstFrame) {
                Ok(_) => true,
                Err(error) => !error.is::<NoFrames>(),
            }
    }

    fn read_take(&self, extent: Extent) -> anyhow::Result<Take> {
        let directory = recall::open_directory(&self.directory)?;
        let opened = match recall::open_file(&directory, LOG, FILE_LIMIT) {
            Err(error) if recall::empty(&error) => anyhow::bail!(NoFrames),
            opened => opened,
        };
        let mut reader = io::BufReader::new(found(opened)?.ok_or(NoFrames)?);
        let mut header = [0; log::HEADER_BYTES];
        anyhow::ensure!(read_complete(&mut reader, &mut header)?, NoFrames);
        let mut take = log::start(&header).map_err(|_damaged| NoFrames)?;
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
            if take.apply(&event).is_err()
                || take.ended()
                || matches!(extent, Extent::FirstFrame) && !take.journal().is_empty()
            {
                break;
            }
        }
        anyhow::ensure!(!take.journal().is_empty(), NoFrames);
        Ok(take)
    }

    pub(crate) fn discard(self) -> anyhow::Result<()> {
        mark_discarded(&self.directory)?;
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
    let marker = Marker::parse(&marker).context(DAMAGED)?;
    let owner = checkpoint.linked(marker).context(DAMAGED)?;
    if owner.is_none_or(|(target, _)| {
        directory.file_name().and_then(|name| name.to_str()) != Some(target.as_str())
    }) {
        return Ok(None);
    }
    checkpoint.discard()?;
    Ok(Some(path))
}

pub(crate) struct Listing {
    pub shows: Vec<Interrupted>,
    pub problems: Vec<Problem>,
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
    let named = |directory: &Path| {
        path == directory || target.to_str().is_some_and(|id| id == label(directory))
    };
    let Listing { shows, problems } = list(parent, SystemTime::now());
    if let Some(show) = shows.into_iter().find(|show| named(&show.directory)) {
        return Ok(show);
    }
    let directory = problems
        .into_iter()
        .map(|problem| problem.directory)
        .find(|directory| named(directory))
        .context(UNMATCHED)?;
    let checkpoint = found(lock(&directory))?.context(UNMATCHED)?.context(BUSY)?;
    anyhow::ensure!(!checkpoint.retired(SystemTime::now())?, UNMATCHED);
    Ok(Interrupted {
        recorded: checkpoint.recorded()?,
        pending: checkpoint.publication()? == Some(Publication::Pending),
        directory,
    })
}

fn label(directory: &Path) -> String {
    format!(
        "interrupted:{}",
        directory.file_name().unwrap_or_default().to_string_lossy()
    )
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

struct Marker {
    target: ShowId,
    publication: Publication,
    superseded: Option<ShowId>,
}

impl Marker {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let marker = std::str::from_utf8(bytes).ok()?;
        let (current, superseded) = match marker.split_once(SUPERSEDES) {
            Some((current, previous)) => (current, Some(ShowId::parse(previous)?)),
            None => (marker, None),
        };
        let (target, publication) = match current.strip_suffix(NARRATED) {
            Some(target) => (target, Publication::Narrated),
            None => (current, Publication::Pending),
        };
        Some(Self {
            target: ShowId::parse(target)?,
            publication,
            superseded,
        })
    }
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

#[derive(Clone, Copy)]
enum Extent {
    Whole,
    FirstFrame,
}

#[derive(Debug)]
struct NoFrames;

impl fmt::Display for NoFrames {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(NO_FRAMES)
    }
}

impl std::error::Error for NoFrames {}

#[cfg_attr(test, derive(Debug, PartialEq))]
pub(crate) enum SavedNarration {
    Absent,
    Damaged,
    Saved(Narration, Option<NarrationLoss>),
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredNarration<N> {
    version: u32,
    narration: N,
    incomplete: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    audio_silent: bool,
}

pub(crate) enum Checkpointing {
    Writing(Keeper),
    Held(Checkpoint),
}

pub(crate) enum Release {
    Discard,
    Keep(Option<(Narration, Option<NarrationLoss>)>),
}

impl Checkpointing {
    pub(crate) async fn close(self) -> anyhow::Result<Option<Checkpoint>> {
        match self {
            Self::Writing(keeper) => keeper.close().await,
            Self::Held(checkpoint) => Ok(Some(checkpoint)),
        }
    }

    pub(crate) async fn release(self, release: Release) -> anyhow::Result<()> {
        if let (Self::Writing(keeper), Release::Discard) = (&self, &release) {
            keeper.discard();
        }
        let Some(checkpoint) = self.close().await? else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || match release {
            Release::Discard => checkpoint.discard(),
            Release::Keep(Some((narration, loss))) => checkpoint.narrate(&narration, loss),
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

fn mark_discarded(directory: &Path) -> io::Result<()> {
    let discarded = directory.join(DISCARDED);
    match File::create_new(&discarded) {
        Ok(file) => file.sync_all()?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            regular(&discarded)?;
            OpenOptions::new()
                .write(true)
                .open(&discarded)?
                .sync_all()?;
        },
        Err(error) => return Err(error),
    }
    sync_directory(directory)
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
