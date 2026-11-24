pub(in crate::show::store) mod audio;
mod keeper;

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Context as _;
pub(crate) use audio::KeptAudio;
pub(crate) use keeper::{AudioEnding, AudioLog, AudioTail, FrameLog, Keeper, Kept};
use speakeasy_core::machine::Release;
use speakeasy_show::{Bundle, Moment, Take, log};

use super::{KEPT_FOR, Staged, age, found, movie};

const ROOT: &str = ".checkpoints";
const LOG: &str = "take.log";
const LOCK: &str = "owner.lock";
const PENDING: &str = "pending";
const GONE: &str = "This interrupted Show expired, was discarded, or was already recovered. \
                   Refresh Shows.";
const NO_FRAMES: &str = "No complete frame survived. Discard this interrupted Show.";
const BUSY: &str = "This Show is already recording, recovering, or being discarded. Refresh Shows.";
const UNMATCHED: &str =
    "No interrupted Show matches that ID or path. Run speakeasy show list --json.";
const VIDEO_LOST: &str = "\n\nVideo could not be saved. The saved keyframes and narration remain \
                          available.\n";

pub(crate) struct Checkpoint {
    directory: PathBuf,
    recorded: SystemTime,
    lock: File,
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
        let checkpoint = found(lock(&self.directory))?.context(GONE)?.context(BUSY)?;
        anyhow::ensure!(!checkpoint.expired(SystemTime::now()), GONE);
        Ok(checkpoint)
    }
}

impl Checkpoint {
    fn create(parent: &Path, recorded: SystemTime) -> anyhow::Result<Self> {
        let root = parent.join(ROOT);
        fs::create_dir_all(&root)?;
        let directory = super::fresh_directory(&root, recorded)?;
        fs::create_dir(&directory)?;
        hold(&directory, File::create_new(directory.join(LOCK))?)?.context(BUSY)
    }

    pub(crate) const fn recorded(&self) -> SystemTime {
        self.recorded
    }

    pub(crate) fn interrupted(self) -> Interrupted {
        Interrupted {
            pending: self.pending(),
            recorded: self.recorded,
            directory: self.directory,
        }
    }

    fn expired(&self, now: SystemTime) -> bool {
        self.recorded
            .checked_add(KEPT_FOR)
            .is_none_or(|expiry| expiry < now)
    }

    pub(in crate::show) fn pending(&self) -> bool {
        self.directory.join(PENDING).exists()
    }

    pub(in crate::show) fn keep_for_retry(&self) -> io::Result<()> {
        fs::write(self.directory.join(PENDING), [])
    }

    pub(in crate::show) fn retain_movie(
        &self,
        staged: &mut Staged,
        annotations: Moment,
    ) -> anyhow::Result<bool> {
        let (note, lost) = match movie::open(&self.directory) {
            Ok(None) => return Ok(false),
            Ok(Some(reading)) => {
                let metadata = movie::retain(staged.staging.path(), reading, annotations)
                    .context("Can't save the recorded video")?;
                (metadata.note(&staged.directory.join(movie::FILE))?, false)
            },
            Err(_) => (VIDEO_LOST.to_owned(), true),
        };
        staged.summary.push_str(&note);
        fs::write(staged.staging.path().join(Bundle::SUMMARY), &staged.summary)?;
        Ok(lost)
    }

    pub(in crate::show) fn retain_voice(&self, staged: &Staged) -> anyhow::Result<()> {
        super::voice::retain(&self.directory.join(audio::FILE), staged)
    }

    pub(crate) fn audio(&self) -> anyhow::Result<Option<KeptAudio>> {
        audio::read(&self.directory.join(audio::FILE))
    }

    pub(in crate::show) fn has_audio(&self) -> bool {
        !matches!(self.audio(), Ok(None))
    }

    pub(crate) fn replay(&self) -> anyhow::Result<Take> {
        let file = found(File::open(self.directory.join(LOG)))?.context(NO_FRAMES)?;
        let mut reader = io::BufReader::new(file);
        let mut header = [0; log::HEADER_BYTES];
        anyhow::ensure!(read_complete(&mut reader, &mut header)?, NO_FRAMES);
        let mut take = log::start(&header).ok().context(NO_FRAMES)?;
        loop {
            let mut length = [0; 4];
            if !read_complete(&mut reader, &mut length)? {
                break;
            }
            let Ok(size) = log::record_size(&length) else {
                break;
            };
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
        let Self {
            directory, lock, ..
        } = self;
        drop(lock);
        fs::remove_dir_all(&directory)
            .context("Can't remove the interrupted Show; close files open in its folder")
    }
}

pub(crate) fn list(parent: &Path, now: SystemTime) -> Vec<Interrupted> {
    let mut shows: Vec<Interrupted> = owned(parent)
        .filter(|checkpoint| !checkpoint.expired(now))
        .map(Checkpoint::interrupted)
        .collect();
    shows.sort_by(|a, b| b.directory.cmp(&a.directory));
    shows
}

pub(crate) fn resolve(parent: &Path, target: &Path) -> anyhow::Result<Interrupted> {
    let path = std::path::absolute(target)?;
    list(parent, SystemTime::now())
        .into_iter()
        .find(|show| path == show.directory || target.to_str() == Some(show.id().as_str()))
        .context(UNMATCHED)
}

pub(super) fn sweep(parent: &Path, now: SystemTime) -> anyhow::Result<()> {
    let mut stuck = None;
    for checkpoint in owned(parent).filter(|checkpoint| checkpoint.expired(now)) {
        let directory = checkpoint.directory.clone();
        if let Err(error) = checkpoint.discard() {
            stuck.get_or_insert((directory, error));
        }
    }
    if let Some((directory, error)) = stuck {
        anyhow::bail!(
            "Can't delete the expired interrupted Show {}: {error:#}. Speakeasy tries again \
             within the hour.",
            directory.display()
        );
    }
    Ok(())
}

fn owned(parent: &Path) -> impl Iterator<Item = Checkpoint> {
    fs::read_dir(parent.join(ROOT))
        .into_iter()
        .flatten()
        .filter_map(|entry| lock(&entry.ok()?.path()).ok().flatten())
}

fn lock(directory: &Path) -> io::Result<Option<Checkpoint>> {
    hold(
        directory,
        OpenOptions::new().write(true).open(directory.join(LOCK))?,
    )
}

fn hold(directory: &Path, lock: File) -> io::Result<Option<Checkpoint>> {
    let recorded = directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(age::recorded_at)
        .ok_or_else(|| io::Error::other("Not an interrupted Show"))?;
    match lock.try_lock() {
        Ok(()) => Ok(Some(Checkpoint {
            directory: directory.to_owned(),
            recorded,
            lock,
        })),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

pub(crate) enum Checkpointing {
    Writing(Keeper),
    Held(Checkpoint),
}

impl Checkpointing {
    pub(crate) async fn close(self) -> anyhow::Result<Option<Checkpoint>> {
        match self {
            Self::Writing(keeper) => keeper.close().await,
            Self::Held(checkpoint) => Ok(Some(checkpoint)),
        }
    }

    pub(crate) async fn release(self, release: Release) -> anyhow::Result<()> {
        if let (Self::Writing(keeper), Release::Discard) = (&self, release) {
            keeper.discard();
        }
        let checkpoint = self.close().await?;
        match (checkpoint, release) {
            (Some(checkpoint), Release::Discard) => {
                tokio::task::spawn_blocking(move || checkpoint.discard())
                    .await
                    .unwrap_or_else(|_panicked| {
                        Err(anyhow::anyhow!(
                            "Show cleanup stopped. Check interrupted Shows."
                        ))
                    })
            },
            (_, Release::Keep) | (None, Release::Discard) => Ok(()),
        }
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
