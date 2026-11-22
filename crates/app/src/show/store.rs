use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use gpui::App;
use speakeasy_show::{Bundle, Journal, Location, Moment, Wsl};

const KEPT_FOR: Duration = Duration::from_hours(7 * 24);

const SWEEP_EVERY: Duration = Duration::from_hours(1);

pub(super) const SAVED_AT: &str = ".saved-at";

const PIN: &str = ".pinned";

pub(super) fn directory() -> anyhow::Result<PathBuf> {
    Ok(crate::paths::data_root()?.join("shows"))
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

pub(super) fn managed<'a>(
    directory: &'a Path,
    wsl: Option<&'a Wsl>,
) -> anyhow::Result<Location<'a>> {
    Location::managed(directory, wsl).context("A Show's folder needs a name made of Unicode text")
}

pub(super) struct Staged {
    staging: tempfile::TempDir,
    directory: PathBuf,
    summary: String,
    managed: bool,
    pub source_lost: bool,
}

impl Staged {
    pub(super) fn persist(mut self) -> anyhow::Result<(SavedShow, String)> {
        let parent = self.directory.parent().unwrap_or(&self.directory);
        if self.managed {
            fs::write(self.staging.path().join(SAVED_AT), []).map_err(cannot_save(parent))?;
        }
        fs::rename(self.staging.path(), &self.directory).map_err(cannot_save(parent))?;
        self.staging.disable_cleanup(true);
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
    tempfile::tempdir_in(parent)
}

pub(super) fn stage(
    location: Location<'_>,
    bundle: Bundle,
    retained: Option<Retained<'_>>,
) -> anyhow::Result<Staged> {
    let directory = location.directory();
    let managed = location.id().is_some();
    let parent = directory
        .parent()
        .context("A Show's directory needs a parent")?;
    let staging = staging(parent).map_err(cannot_save(parent))?;
    let source_lost = retained.as_ref().is_some_and(|retained| {
        source::write(staging.path(), retained.journal, retained.duration).is_err()
    });
    let command = retained
        .and_then(|retained| retained.command)
        .filter(|_| !source_lost);
    let (summary, files) = bundle.into_files(location, command)?;
    for file in files {
        let path = staging.path().join(&file.name);
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder).map_err(cannot_save(parent))?;
        }
        fs::write(path, &file.contents).map_err(cannot_save(parent))?;
    }
    Ok(Staged {
        staging,
        directory: directory.to_path_buf(),
        summary,
        managed,
        source_lost,
    })
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
    let checkpoints = checkpoint::sweep(parent, now);
    let cutoff = utc_stamp(now.checked_sub(KEPT_FOR).unwrap_or(SystemTime::UNIX_EPOCH));
    let shows =
        recall::directories(parent).with_context(|| format!("Cannot read {}", parent.display()))?;
    let mut stuck = None;
    for show in shows {
        let due = show
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(stamp)
            .is_some_and(|stamp| stamp < cutoff.as_str());
        if due
            && !pinned(&show).unwrap_or(true)
            && let Err(error) = fs::remove_dir_all(&show)
        {
            stuck.get_or_insert((show, error));
        }
    }
    if let Some((show, error)) = stuck {
        anyhow::bail!(
            "Cannot delete the expired Show {}: {error}. Close its open files; Speakeasy tries \
             again within the hour.",
            show.display()
        );
    }
    checkpoints
}

pub(super) fn pinned(path: &Path) -> io::Result<bool> {
    fs::exists(path.join(PIN))
}

fn found<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    found(fs::remove_file(path))?;
    Ok(())
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
    let [year, month, day, hour, minute, second] = utc_fields(now);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

fn utc_fields(at: SystemTime) -> [u64; 6] {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (days, time) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_date(days);
    [year, month, day, time / 3_600, time / 60 % 60, time % 60]
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
