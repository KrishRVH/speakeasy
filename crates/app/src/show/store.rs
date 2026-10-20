//! Where Shows live on disk: stamp naming, private all-at-once writes, and seven-day expiry.

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use gpui::App;

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

/// Where Shows live: machine-local, beside the engines and models.
fn directory() -> anyhow::Result<PathBuf> {
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
        .with_context(|| format!("Cannot create {}", directory.display()))?;
    let linked = fs::symlink_metadata(directory)
        .with_context(|| format!("Cannot read {}", directory.display()))?
        .file_type()
        .is_symlink();
    anyhow::ensure!(
        !linked,
        "{} is a link; Shows need a folder of their own, so replace the link with one",
        directory.display()
    );
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("Cannot make {} private", directory.display()))
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
}

impl Staged {
    /// Moves the bundle into place.
    pub(super) fn persist(mut self) -> anyhow::Result<()> {
        fs::rename(self.staging.path(), &self.directory)
            .with_context(|| format!("Cannot create {}", self.directory.display()))?;
        // The staging directory is now the bundle.
        self.staging.disable_cleanup(true);
        Ok(())
    }
}

/// Writes `files` beside `directory`, which must not exist yet. On Unix the bundle is the user's
/// alone; on Windows it takes its parent's access, which for the Shows folder is the user's alone
/// (see [`private_directory`]).
pub(super) fn stage(directory: &Path, files: &[speakeasy_show::File]) -> anyhow::Result<Staged> {
    anyhow::ensure!(
        !directory.exists(),
        "{} already exists, and a Show never replaces one",
        directory.display()
    );
    let parent = directory
        .parent()
        .context("A Show's directory needs a parent")?;
    fs::create_dir_all(parent).with_context(|| format!("Cannot create {}", parent.display()))?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(STAGING_PREFIX);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let staging = builder
        .tempdir_in(parent)
        .with_context(|| format!("Cannot write in {}", parent.display()))?;
    for file in files {
        let path = staging.path().join(&file.name);
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)
                .with_context(|| format!("Cannot create {}", folder.display()))?;
        }
        fs::write(&path, &file.contents)
            .with_context(|| format!("Cannot write {}", path.display()))?;
    }
    Ok(Staged {
        staging,
        directory: directory.to_path_buf(),
    })
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
/// never finished. Only directories named the way Speakeasy names them are touched.
fn sweep(parent: &Path, now: SystemTime) -> anyhow::Result<()> {
    let entries = match fs::read_dir(parent) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        entries => entries.with_context(|| format!("Cannot read {}", parent.display()))?,
    };
    let cutoff = utc_stamp(now.checked_sub(KEPT_FOR).unwrap_or(SystemTime::UNIX_EPOCH));
    let mut stuck = false;
    for entry in entries.flatten() {
        // The entry's own type: a link is never followed out of the directory.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        let expired = if name.starts_with(STAGING_PREFIX) {
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| {
                    now.duration_since(modified)
                        .is_ok_and(|age| age >= ABANDONED_AFTER)
                })
        } else {
            // Stamps sort in time order, so comparing them compares when Shows were recorded.
            stamp(name).is_some_and(|stamp| stamp < cutoff.as_str()) && !pinned(&path)
        };
        if expired && fs::remove_dir_all(&path).is_err() {
            stuck = true;
        }
    }
    anyhow::ensure!(
        !stuck,
        "Could not delete every expired Show in {}. Close any of their files that are open; \
         Speakeasy tries again within the hour.",
        parent.display()
    );
    Ok(())
}

/// Whether the Show at `path` holds a pin. Only a pin known to be absent lets it expire, and a
/// pin that is itself a link still counts.
fn pinned(path: &Path) -> bool {
    !matches!(
        fs::symlink_metadata(path.join(PIN)),
        Err(error) if error.kind() == ErrorKind::NotFound
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))
            .unwrap()
    }

    #[test]
    fn a_week_old_show_expires_unless_pinned() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let parent = root.path();
        let now = SystemTime::now();
        let days_ago = |days: u64| utc_stamp(now - Duration::from_hours(days * 24));
        // Three Shows the writer named in one second. The bare `<stamp>` and `<stamp>-2` expire, so
        // the sweep must read both name forms; the pinned `<stamp>-3` stays.
        let written = || -> anyhow::Result<String> {
            let show = fresh_directory(parent, now - Duration::from_hours(8 * 24))?;
            fs::create_dir(&show)?;
            Ok(show
                .file_name()
                .context("A Show has a name")?
                .to_string_lossy()
                .into_owned())
        };
        written()?;
        written()?;
        let pinned = written()?;
        let staging = format!("{STAGING_PREFIX}1");
        for name in [days_ago(6), "notes".into(), staging.clone()] {
            fs::create_dir(parent.join(name))?;
        }
        fs::write(parent.join(&pinned).join(PIN), b"")?;
        fs::write(parent.join(days_ago(9)), b"not a Show")?;
        // A link named like an expired Show is left alone, and so is what it points to.
        #[cfg(unix)]
        let elsewhere = {
            let elsewhere = tempfile::tempdir()?;
            std::os::unix::fs::symlink(elsewhere.path(), parent.join(days_ago(10)))?;
            elsewhere
        };
        let names = || -> anyhow::Result<Vec<String>> {
            let mut names = fs::read_dir(parent)?
                .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
                .collect::<anyhow::Result<Vec<_>>>()?;
            names.sort();
            Ok(names)
        };

        sweep(parent, now)?;
        let mut kept = vec![
            staging.clone(),
            days_ago(9),
            days_ago(6),
            pinned,
            "notes".into(),
        ];
        #[cfg(unix)]
        {
            kept.push(days_ago(10));
            assert!(elsewhere.path().exists());
        }
        kept.sort();
        assert_eq!(names()?, kept);

        sweep(parent, now + 2 * ABANDONED_AFTER)?;
        kept.retain(|name| *name != staging);
        assert_eq!(names()?, kept);

        sweep(&parent.join("missing"), now)
    }

    #[test]
    fn only_names_speakeasy_writes_are_stamps() {
        assert_eq!(stamp("20261019T143603Z"), Some("20261019T143603Z"));
        assert_eq!(stamp("20261019T143603Z-12"), Some("20261019T143603Z"));
        for name in [
            "20261019T143603",
            "20261019T143603Z-",
            "20261019T143603Z-a",
            "20261019T143603Zcopy",
            "2026-10-19T14360Z",
            "notes",
        ] {
            assert_eq!(stamp(name), None, "{name}");
        }
    }

    #[test]
    fn stamps_name_utc_moments_in_order() {
        assert_eq!(utc_stamp(at(0)), "19700101T000000Z");
        assert_eq!(utc_stamp(at(951_782_400)), "20000229T000000Z");
        assert_eq!(utc_stamp(at(1_792_420_563)), "20261019T143603Z");
        assert_eq!(utc_stamp(at(4_102_444_799)), "20991231T235959Z");
    }

    #[test]
    fn a_bundle_appears_whole_under_a_fresh_name() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let parent = root.path().join("shows");
        let first = fresh_directory(&parent, at(1_792_420_563))?;
        let files = [
            speakeasy_show::File {
                name: "summary.md".into(),
                contents: b"text".to_vec(),
            },
            speakeasy_show::File {
                name: "frames/frame-01.png".into(),
                contents: vec![1, 2],
            },
        ];
        stage(&first, &files)?.persist()?;
        assert_eq!(fs::read(first.join("frames/frame-01.png"))?, [1, 2]);
        assert!(
            stage(&first, &files).is_err(),
            "an existing bundle is never replaced"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(&first)?.permissions().mode() & 0o777, 0o700);
        }
        let second = fresh_directory(&parent, at(1_792_420_563))?;
        assert_eq!(second.file_name(), Some("20261019T143603Z-2".as_ref()));
        let names: Vec<_> = fs::read_dir(&parent)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<_, _>>()?;
        assert_eq!(names, ["20261019T143603Z"]);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn the_shows_folder_is_made_private_and_a_link_is_refused() -> anyhow::Result<()> {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let mode = |path: &Path| -> anyhow::Result<u32> {
            Ok(fs::metadata(path)?.permissions().mode() & 0o777)
        };
        let root = tempfile::tempdir()?;
        let created = root.path().join("speakeasy/shows");
        make_private(&created)?;
        assert_eq!(mode(&created)?, 0o700);

        let existing = root.path().join("existing");
        fs::create_dir(&existing)?;
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o755))?;
        let link = root.path().join("link");
        symlink(&existing, &link)?;
        assert!(make_private(&link).is_err(), "a link is never followed");
        assert_eq!(mode(&existing)?, 0o755, "what a link names is left alone");
        make_private(&existing)?;
        assert_eq!(mode(&existing)?, 0o700, "an existing folder is tightened");
        Ok(())
    }
}
