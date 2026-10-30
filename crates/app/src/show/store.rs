//! Where Shows live on disk: stamp naming, private all-at-once writes, and seven-day expiry.

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
use speakeasy_show::Bundle;

/// How long an unpinned Show is kept.
const KEPT_FOR: Duration = Duration::from_hours(7 * 24);
/// A staging directory this old belongs to a publication that never finished, such as one cut
/// short by a crash; publishing takes seconds.
const ABANDONED_AFTER: Duration = Duration::from_hours(1);
/// How often a long-running app looks for expired Shows.
const SWEEP_EVERY: Duration = Duration::from_hours(1);
/// A file of this name inside a Show keeps it.
const PIN: &str = ".pinned";
/// Begins the name of the hidden directory a bundle is staged in, which the sweep deletes once
/// abandoned.
const STAGING_PREFIX: &str = ".partial-";
/// Expired Shows leave the visible library before recursive deletion, which can fail partway.
const EXPIRED_PREFIX: &str = ".expired-";

/// Where Shows live: machine-local, beside the engines and models.
pub(super) fn directory() -> anyhow::Result<PathBuf> {
    Ok(crate::paths::data_root()?.join("shows"))
}

/// [`directory`], ready for a new Show. On Unix it is created or tightened to be the user's alone,
/// since even a Show's name tells when the user recorded; Windows already limits local application
/// data to the user.
pub(super) fn private_directory() -> anyhow::Result<PathBuf> {
    let directory = directory()?;
    #[cfg(unix)]
    make_private(&directory)?;
    Ok(directory)
}

/// Creates `directory` if needed and leaves it readable only by the user. A link is refused rather
/// than followed, so only a folder Speakeasy owns has its permissions changed.
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

/// An unused directory under `parent` named for `now` in UTC, such as `20261019T143603Z`.
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

/// A bundle written to a hidden sibling of its directory. It appears there only once persisted,
/// so a reader never sees half a bundle, and dropping it unpersisted deletes it.
pub(super) struct Staged {
    staging: tempfile::TempDir,
    directory: PathBuf,
    /// The summary written in the bundle, naming its files where it appears.
    summary: String,
}

impl Staged {
    /// Adds the retained source to this private staging directory. A failed source write leaves
    /// the staging owner responsible for cleanup; the visible bundle is never partial.
    pub(super) fn write_source(
        &self,
        journal: &speakeasy_show::Journal,
        duration: speakeasy_show::Moment,
    ) -> anyhow::Result<()> {
        source::write(self.staging.path(), journal, duration)
            .context("Cannot save the Show's source frames")
    }

    /// Moves the bundle into place and returns the saved Show with the summary written in it.
    pub(super) fn persist(mut self) -> anyhow::Result<(SavedShow, String)> {
        let parent = self.directory.parent().unwrap_or(&self.directory);
        fs::rename(self.staging.path(), &self.directory).map_err(cannot_save(parent))?;
        self.staging.disable_cleanup(true);
        #[cfg(unix)]
        {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Rename already made the complete bundle visible; a failed directory sync cannot turn that saved Show into a failed publication"
            )]
            let _ = fs::File::open(parent).and_then(|file| file.sync_all());
        }
        Ok((SavedShow(self.directory.into()), self.summary))
    }
}

/// A Show whose bundle was persisted, known only by its folder: no transcript or summary text
/// stays in memory, and what is read back is whatever the folder holds then. The path never
/// changes, so every snapshot that presents the Show shares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedShow(Arc<Path>);

impl SavedShow {
    /// Stable identity only; callers perform any existence check off the UI thread.
    pub(crate) fn directory(&self) -> &Path {
        &self.0
    }

    /// Trusts `directory` as a saved Show's folder, for tests whose Shows are never written.
    #[cfg(test)]
    pub(crate) fn assume(directory: impl Into<PathBuf>) -> Self {
        Self(directory.into().into())
    }
}

/// Explains a failed write of a Show into the Shows folder `parent`, naming that folder rather than
/// the hidden staging one, with the system's reason and what to do.
fn cannot_save(parent: &Path) -> impl FnOnce(io::Error) -> anyhow::Error {
    move |error| {
        anyhow::anyhow!(
            "Cannot save the Show in {}: {error}. {}",
            parent.display(),
            crate::paths::remedy(&error)
        )
    }
}

/// Writes `bundle` beside `directory`, which must not exist yet, with a summary naming its files
/// there. On Unix the bundle is the user's alone; on Windows it takes its parent's access, which
/// for the Shows folder is the user's alone (see [`private_directory`]).
pub(super) fn stage(directory: &Path, bundle: Bundle) -> anyhow::Result<Staged> {
    anyhow::ensure!(
        !directory.exists(),
        "{} already exists, and a Show never replaces one",
        directory.display()
    );
    let parent = directory
        .parent()
        .context("A Show's directory needs a parent")?;
    let (summary, files) = bundle.into_files(directory)?;
    fs::create_dir_all(parent).map_err(cannot_save(parent))?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(STAGING_PREFIX);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let staging = builder.tempdir_in(parent).map_err(cannot_save(parent))?;
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
    // Flushed before the rename, so a crash leaves either no bundle or all of it. On Unix a
    // folder's entries survive a crash only once the folder itself is flushed.
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
    })
}

/// Writes a new file holding `contents` and flushes it to disk.
fn write_flushed(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create_new(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

/// Deletes expired Shows now and then hourly for the app's life, giving each failure to `report`;
/// a failure that persists is reported again at every sweep.
pub(crate) fn attend(report: impl Fn(&anyhow::Error, &mut App) + 'static, cx: &App) {
    // Without a data folder no Show can be published, so none can expire.
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
            if let Err(error) = swept
                && cx.update(|cx| report(&error, cx)).is_err()
            {
                return;
            }
            cx.background_executor().timer(SWEEP_EVERY).await;
        }
    })
    .detach();
}

/// Deletes the Shows under `parent` that expired by `now`, and staging left by publications that
/// never finished. Only directories named the way Speakeasy names them are touched. An entry that
/// cannot be inspected is kept and fails the sweep, which goes on to expire the rest.
fn sweep(parent: &Path, now: SystemTime) -> anyhow::Result<()> {
    let entries = match fs::read_dir(parent) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        entries => entries.with_context(|| format!("Cannot read {}", parent.display()))?,
    };
    let checkpoint_error = checkpoint::sweep(parent, now).err();
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
            // An entry renamed or deleted since the listing has nothing left to expire.
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
    if let Some(error) = checkpoint_error {
        return Err(error);
    }
    Ok(())
}

enum Cleanup {
    Keep,
    Remove(PathBuf),
    Blocked,
}

/// The lock orders Keep against the final pin check and rename, across app and CLI processes.
/// Recursive removal happens after release: an open file cannot stall another Show's Keep.
fn retire_expired(
    parent: &Path,
    entry: &fs::DirEntry,
    cutoff: &str,
    now: SystemTime,
) -> io::Result<Cleanup> {
    let _lock = retention::lock(parent)?;
    if !expired(entry, cutoff, now)? {
        return Ok(Cleanup::Keep);
    }
    let path = entry.path();
    let name = entry.file_name();
    if name.to_str().is_some_and(|name| stamp(name).is_some()) {
        let destination = parent.join(format!("{EXPIRED_PREFIX}{}", name.to_string_lossy()));
        match fs::symlink_metadata(&destination) {
            Ok(_) => return Ok(Cleanup::Blocked),
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
        match fs::rename(path, &destination) {
            Ok(()) => Ok(Cleanup::Remove(destination)),
            Err(error) if error.kind() == ErrorKind::NotFound => Err(error),
            Err(_blocked) => Ok(Cleanup::Blocked),
        }
    } else {
        Ok(Cleanup::Remove(path))
    }
}

/// Whether `entry`, judged by its own type so a link is never followed out of the folder, is a
/// directory holding an unpinned Show stamped before `cutoff`, staging abandoned by `now`, or
/// the remainder of a previously retired Show.
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
        // Staging modified after `now` is the clock stepping back, not abandonment.
        Ok(now
            .duration_since(modified)
            .is_ok_and(|age| age >= ABANDONED_AFTER))
    } else {
        Ok(stamp(name).is_some_and(|stamp| stamp < cutoff) && !pinned(&entry.path())?)
    }
}

/// Whether the Show at `path` holds a pin. Only a pin known to be absent lets it expire, and a
/// pin that is itself a link still counts.
pub(super) fn pinned(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path.join(PIN)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// The stamp a Show's directory is named for, such as `20261019T143603Z` in `20261019T143603Z-2`.
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

/// `now` in UTC as `YYYYMMDDTHHMMSSZ`, which sorts in time order.
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

/// The proleptic Gregorian date `days` after 1970-01-01, after Howard Hinnant's `civil_from_days`.
fn civil_date(days: u64) -> (u64, u64, u64) {
    // Count years from March, so each year's leap day falls at its end.
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
pub(super) mod recall;
pub(super) mod retention;
pub(super) mod source;
#[cfg(test)]
mod tests;
