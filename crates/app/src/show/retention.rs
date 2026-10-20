//! Shows expire a week after they are recorded unless pinned, so screen recordings never pile up
//! unnoticed.

use std::{
    fs,
    io::ErrorKind,
    path::Path,
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use gpui::App;

use super::utc_stamp;

/// How long an unpinned Show is kept.
const KEPT_FOR: Duration = Duration::from_hours(7 * 24);
/// A staging directory this old belongs to a publication that never finished, such as one cut
/// short by a crash; publishing takes seconds.
const ABANDONED_AFTER: Duration = Duration::from_hours(1);
/// How often a long-running app looks for expired Shows.
const SWEEP_EVERY: Duration = Duration::from_hours(1);
/// A file of this name inside a Show keeps it.
const PIN: &str = ".pinned";

/// Deletes expired Shows now and then hourly for the app's life, giving each failure to `report`;
/// a failure that persists is reported again at every sweep.
pub(crate) fn attend(report: impl Fn(&anyhow::Error, &mut App) + 'static, cx: &App) {
    cx.spawn(async move |cx| {
        loop {
            let swept = cx
                .background_executor()
                .spawn(async { sweep(&super::directory(), SystemTime::now()) })
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
    let mut stuck = 0_usize;
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
        let expired = if name.starts_with(".partial-") {
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
            stuck = stuck.saturating_add(1);
        }
    }
    anyhow::ensure!(
        stuck == 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_week_old_show_expires_unless_pinned() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let parent = root.path();
        let now = SystemTime::now();
        let days_ago = |days: u64| utc_stamp(now - Duration::from_hours(days * 24));
        let pinned = format!("{}-2", days_ago(8));
        for name in [
            days_ago(8),
            pinned.clone(),
            days_ago(6),
            "notes".into(),
            ".partial-1".into(),
        ] {
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
            ".partial-1".to_owned(),
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
        kept.retain(|name| name != ".partial-1");
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
}
