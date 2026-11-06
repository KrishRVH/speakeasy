use std::{
    collections::BTreeSet,
    fs,
    io::{self, ErrorKind, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use gpui::App;
use speakeasy_show::{Bundle, Journal, Moment, Retrieval};

const KEPT_FOR: Duration = Duration::from_hours(7 * 24);

const ABANDONED_AFTER: Duration = Duration::from_hours(1);

const SWEEP_EVERY: Duration = Duration::from_hours(1);

const PIN: &str = ".pinned";

const STAGING_PREFIX: &str = ".partial-";

const EXPIRED_PREFIX: &str = ".expired-";

pub(super) fn directory() -> anyhow::Result<PathBuf> {
    Ok(crate::paths::data_root()?.join("shows"))
}

pub(super) fn private_directory() -> anyhow::Result<PathBuf> {
    let directory = directory()?;
    #[cfg(unix)]
    make_private(&directory)?;
    Ok(directory)
}

#[cfg(unix)]
fn make_private(directory: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(cannot_save(directory))?;
    let linked = fs::symlink_metadata(directory)
        .map_err(cannot_save(directory))?
        .file_type()
        .is_symlink();
    anyhow::ensure!(
        !linked,
        "{} is a link; Shows need a folder of their own, so replace the link with one",
        directory.display()
    );
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .map_err(cannot_save(directory))
}

pub(super) fn fresh_directory(parent: &Path, now: SystemTime) -> anyhow::Result<PathBuf> {
    let stamp = utc_stamp(now);
    (1..=100_u8)
        .map(|attempt| match attempt {
            1 => parent.join(&stamp),
            _ => parent.join(format!("{stamp}-{attempt}")),
        })
        .find(|candidate| !candidate.exists())
        .context("Too many Shows were written this second")
}

pub(super) struct Staged {
    staging: tempfile::TempDir,
    directory: PathBuf,
    summary: String,
    pub source_lost: bool,
}

impl Staged {
    fn retain_movie(&mut self, checkpoint: &Path, annotations: Moment) -> anyhow::Result<bool> {
        let lost = match movie::checkpoint(checkpoint)? {
            movie::CheckpointMovie::Absent => return Ok(false),
            movie::CheckpointMovie::Unavailable => {
                self.summary.push_str("\n\nVideo could not be saved: no complete fragment survived. The saved keyframes and narration remain available.\n");
                true
            },
            movie::CheckpointMovie::Present(movie) => {
                let metadata = movie::retain(self.staging.path(), *movie, annotations).context(
                    "Cannot retain the recorded movie; its checkpoint remains available",
                )?;
                self.summary
                    .push_str(&metadata.note(&self.directory.join(movie::FILE))?);
                false
            },
        };
        let summary = self.staging.path().join(Bundle::SUMMARY);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(summary)?;
        file.write_all(self.summary.as_bytes())?;
        file.sync_all()?;
        sync_directory(self.staging.path())?;
        Ok(lost)
    }

    pub(super) fn persist(mut self) -> anyhow::Result<(SavedShow, String)> {
        let parent = self.directory.parent().unwrap_or(&self.directory);
        fs::rename(self.staging.path(), &self.directory).map_err(cannot_save(parent))?;
        self.staging.disable_cleanup(true);
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Rename already made the complete bundle visible; a failed directory sync cannot turn that saved Show into a failed publication"
        )]
        let _ = sync_directory(parent);
        Ok((SavedShow(self.directory.into()), self.summary))
    }
}

pub(super) struct Retained<'a> {
    pub journal: &'a Journal,
    pub duration: Moment,
    pub command: Option<&'a Path>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedShow(Arc<Path>);

impl SavedShow {
    pub(crate) fn directory(&self) -> &Path {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn assume(directory: impl Into<PathBuf>) -> Self {
        Self(directory.into().into())
    }
}

fn cannot_save(parent: &Path) -> impl FnOnce(io::Error) -> anyhow::Error {
    move |error| {
        anyhow::anyhow!(
            "Cannot save the Show in {}: {error}. {}",
            parent.display(),
            crate::paths::remedy(&error)
        )
    }
}

fn staging(parent: &Path) -> io::Result<tempfile::TempDir> {
    fs::create_dir_all(parent)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(STAGING_PREFIX);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent)
}

pub(super) fn stage(
    directory: &Path,
    bundle: Bundle,
    retained: Option<Retained<'_>>,
) -> anyhow::Result<Staged> {
    anyhow::ensure!(
        !directory.exists(),
        "{} already exists, and a Show never replaces one",
        directory.display()
    );
    let parent = directory
        .parent()
        .context("A Show's directory needs a parent")?;
    let staging = staging(parent).map_err(cannot_save(parent))?;
    let source_lost = retained.as_ref().is_some_and(|retained| {
        source::write(staging.path(), retained.journal, retained.duration).is_err()
    });
    let retrieval = retained.filter(|_| !source_lost).and_then(|retained| {
        Some(Retrieval {
            command: retained.command?,
            id: directory.file_name()?.to_str()?,
        })
    });
    let (summary, files) = bundle.into_files(directory, retrieval)?;
    let folders: BTreeSet<PathBuf> = files
        .iter()
        .filter_map(|file| Some(staging.path().join(&file.name).parent()?.to_owned()))
        .collect();
    for folder in &folders {
        fs::create_dir_all(folder).map_err(cannot_save(parent))?;
    }
    for file in files {
        write_flushed(&staging.path().join(&file.name), &file.contents)
            .map_err(cannot_save(parent))?;
    }

    #[cfg(unix)]
    for folder in folders {
        fs::File::open(folder)
            .and_then(|folder| folder.sync_all())
            .map_err(cannot_save(parent))?;
    }
    Ok(Staged {
        staging,
        directory: directory.to_path_buf(),
        summary,
        source_lost,
    })
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    fs::File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Callers share Unix's fallible contract; portable Rust cannot sync a Windows directory handle"
)]
const fn sync_directory(_: &Path) -> io::Result<()> {
    Ok(())
}

fn write_flushed(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create_new(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

pub(crate) fn attend(report: impl Fn(&anyhow::Error, &mut App) + 'static, cx: &App) {
    let Ok(parent) = directory() else {
        return;
    };
    cx.spawn(async move |cx| {
        loop {
            let parent = parent.clone();
            let swept = cx
                .background_executor()
                .spawn(async move { sweep(&parent, SystemTime::now()) })
                .await;
            if let Err(error) = swept {
                cx.update(|cx| report(&error, cx));
            }
            cx.background_executor().timer(SWEEP_EVERY).await;
        }
    })
    .detach();
}

pub(super) fn sweep(parent: &Path, now: SystemTime) -> anyhow::Result<()> {
    let entries = match fs::read_dir(parent) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        entries => entries.with_context(|| format!("Cannot read {}", parent.display()))?,
    };
    let checkpoints = checkpoint::sweep(parent, now);
    let cutoff = utc_stamp(now.checked_sub(KEPT_FOR).unwrap_or(SystemTime::UNIX_EPOCH));
    let mut unchecked = None;
    let mut stuck = false;
    for entry in entries {
        match entry.and_then(|entry| retire_expired(parent, &entry, &cutoff, now)) {
            Ok(Cleanup::Remove(show)) => {
                if let Err(error) = fs::remove_dir_all(show)
                    && error.kind() != ErrorKind::NotFound
                {
                    stuck = true;
                }
            },
            Ok(Cleanup::Keep) => {},
            Ok(Cleanup::Blocked) => stuck = true,
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => {
                unchecked.get_or_insert(error);
            },
        }
    }
    anyhow::ensure!(
        !stuck,
        "Cannot delete every expired Show in {}. Close any of their open files; \
         Speakeasy tries again within the hour.",
        parent.display()
    );
    if let Some(error) = unchecked {
        anyhow::bail!(
            "Cannot check every Show in {} for expiry: {error}. {}",
            parent.display(),
            crate::paths::remedy(&error)
        );
    }
    checkpoints
}

enum Cleanup {
    Keep,
    Remove(PathBuf),
    Blocked,
}

fn retire_expired(
    parent: &Path,
    entry: &fs::DirEntry,
    cutoff: &str,
    now: SystemTime,
) -> io::Result<Cleanup> {
    let _lock = retention::lock(parent)?;
    let path = entry.path();
    if !expired(entry, cutoff, now)? {
        if entry.file_type()?.is_dir() {
            voice::expire(&path, now)?;
        }
        return Ok(Cleanup::Keep);
    }
    let name = entry.file_name();
    let Some(name) = name.to_str().filter(|name| stamp(name).is_some()) else {
        return Ok(Cleanup::Remove(path));
    };
    match retire(parent, &path, name) {
        Ok(destination) => Ok(Cleanup::Remove(destination)),
        Err(error) if error.kind() == ErrorKind::NotFound => Err(error),
        Err(_blocked) => {
            voice::expire(&path, now)?;
            Ok(Cleanup::Blocked)
        },
    }
}

fn retire(parent: &Path, directory: &Path, name: &str) -> io::Result<PathBuf> {
    let destination = parent.join(format!("{EXPIRED_PREFIX}{name}"));
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == ErrorKind::NotFound => {},
        Err(error) => return Err(error),
    }
    fs::rename(directory, &destination)?;
    Ok(destination)
}

fn expired(entry: &fs::DirEntry, cutoff: &str, now: SystemTime) -> io::Result<bool> {
    if !entry.file_type()?.is_dir() {
        return Ok(false);
    }
    let name = entry.file_name();
    let Some(name) = name.to_str() else {
        return Ok(false);
    };
    if name
        .strip_prefix(EXPIRED_PREFIX)
        .is_some_and(|name| stamp(name).is_some())
    {
        return Ok(true);
    }
    if name.starts_with(STAGING_PREFIX) {
        let modified = entry.metadata()?.modified()?;

        Ok(now
            .duration_since(modified)
            .is_ok_and(|age| age >= ABANDONED_AFTER))
    } else {
        Ok(stamp(name).is_some_and(|stamp| stamp < cutoff) && !pinned(&entry.path())?)
    }
}

pub(super) fn pinned(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path.join(PIN)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn stamp(name: &str) -> Option<&str> {
    let (stamp, suffix) = name.split_at_checked(16)?;
    let stamped = stamp.bytes().enumerate().all(|(index, byte)| match index {
        8 => byte == b'T',
        15 => byte == b'Z',
        _ => byte.is_ascii_digit(),
    });
    let numbered = suffix.is_empty()
        || suffix
            .strip_prefix('-')
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()));
    (stamped && numbered).then_some(stamp)
}

fn utc_stamp(now: SystemTime) -> String {
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (days, time) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_date(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        time / 3_600,
        time / 60 % 60,
        time % 60
    )
}

fn civil_date(days: u64) -> (u64, u64, u64) {
    let shifted = days.saturating_add(719_468);
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era = day_of_era
        .saturating_sub(day_of_era / 1_460)
        .saturating_add(day_of_era / 36_524)
        .saturating_sub(day_of_era / 146_096)
        / 365;
    let day_of_year = day_of_era.saturating_sub(
        year_of_era
            .saturating_mul(365)
            .saturating_add(year_of_era / 4)
            .saturating_sub(year_of_era / 100),
    );
    let month_index = day_of_year.saturating_mul(5).saturating_add(2) / 153;
    let day = day_of_year
        .saturating_sub(month_index.saturating_mul(153).saturating_add(2) / 5)
        .saturating_add(1);
    let month = if month_index < 10 {
        month_index.saturating_add(3)
    } else {
        month_index.saturating_sub(9)
    };
    let year = era
        .saturating_mul(400)
        .saturating_add(year_of_era)
        .saturating_add(u64::from(month <= 2));
    (year, month, day)
}

pub(super) mod address;
pub(super) mod age;
pub(crate) mod checkpoint;
pub(super) mod export;
pub(super) mod movie;
pub(super) mod recall;
pub(super) mod retention;
pub(super) mod source;
#[cfg(test)]
mod tests;
pub(super) mod video;
pub(super) mod voice;
