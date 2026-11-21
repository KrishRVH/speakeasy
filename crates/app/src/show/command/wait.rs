use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::Context as _;

use crate::show::store::{
    SAVED_AT,
    recall::{self, Entry},
};

pub(super) const PATIENCE: Duration = Duration::from_secs(600);
pub(super) const LONGEST: Duration = Duration::from_secs(3_600);
const POLL: Duration = Duration::from_millis(500);

pub(super) fn next_saved(parent: &Path, patience: Duration) -> anyhow::Result<Entry> {
    next_saved_polling(parent, patience, POLL, thread::sleep)
}

fn next_saved_polling(
    parent: &Path,
    patience: Duration,
    poll: Duration,
    mut pause: impl FnMut(Duration),
) -> anyhow::Result<Entry> {
    let started = Instant::now();
    let known: HashSet<PathBuf> = shows(parent)?.into_iter().collect();
    loop {
        let fresh = shows(parent)?
            .into_iter()
            .filter(|directory| !known.contains(directory))
            .map(|directory| (saved_at(&directory), directory))
            .collect();
        if let Some(entry) = first_saved(parent, fresh)? {
            return Ok(entry);
        }
        let waited = started.elapsed();
        anyhow::ensure!(
            waited < patience,
            "No new Show was saved within {} seconds. Make one with the Show gesture, then run show wait again.",
            patience.as_secs()
        );
        pause(poll.min(patience.saturating_sub(waited)));
    }
}

fn first_saved(
    parent: &Path,
    mut newest_first: Vec<(Option<SystemTime>, PathBuf)>,
) -> anyhow::Result<Option<Entry>> {
    newest_first.sort_by_key(|&(saved, _)| saved);
    newest_first
        .into_iter()
        .find_map(|(_, directory)| {
            let opened = Entry::open(&directory, Some(parent));
            match opened {
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                opened => Some(
                    opened.with_context(|| format!("Cannot open Show {}", directory.display())),
                ),
            }
        })
        .transpose()
}

fn saved_at(directory: &Path) -> Option<SystemTime> {
    let marker = fs::symlink_metadata(directory.join(SAVED_AT))
        .ok()
        .filter(fs::Metadata::is_file);
    marker
        .or_else(|| fs::metadata(directory).ok())?
        .modified()
        .ok()
}

fn shows(parent: &Path) -> anyhow::Result<Vec<PathBuf>> {
    recall::directories(parent)
        .with_context(|| format!("Cannot list Shows in {}", parent.display()))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use speakeasy_show::{Demo, Typeface};

    use super::*;
    use crate::show::store;

    fn save(parent: &Path, recorded: SystemTime) -> anyhow::Result<PathBuf> {
        let directory = store::fresh_directory(parent, recorded)?;
        let bundle = Demo::new(Typeface::parse(crate::BRAND_FONT_FILE)?).bundle((320, 200))?;
        store::stage(store::managed(&directory, None)?, bundle, None)?.persist()?;
        Ok(directory)
    }

    fn save_at(parent: &Path, recorded: SystemTime, saved: SystemTime) -> anyhow::Result<PathBuf> {
        let directory = save(parent, recorded)?;
        fs::File::options()
            .write(true)
            .open(directory.join(SAVED_AT))?
            .set_modified(saved)?;
        Ok(directory)
    }

    fn wait_for(
        parent: &Path,
        shows: &[(SystemTime, SystemTime)],
    ) -> anyhow::Result<(PathBuf, Vec<PathBuf>)> {
        let mut saved = Vec::new();
        let found = next_saved_polling(parent, Duration::from_secs(20), POLL, |_| {
            if saved.is_empty() {
                saved = shows
                    .iter()
                    .map(|&(recorded, at)| save_at(parent, recorded, at).unwrap())
                    .collect();
            }
        })?;
        Ok((found.directory, saved))
    }

    #[test]
    fn wait_reports_only_a_show_saved_after_it_began() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let earlier = save(root.path(), SystemTime::now())?;
        let (began, waiting) = mpsc::channel();
        let parent = root.path().to_owned();
        let waiter = thread::spawn(move || {
            began.send(())?;
            next_saved_polling(
                &parent,
                Duration::from_secs(20),
                Duration::from_millis(10),
                thread::sleep,
            )
        });
        waiting.recv()?;
        thread::sleep(Duration::from_millis(100));
        let fresh = save(root.path(), SystemTime::now())?;
        let found = waiter
            .join()
            .map_err(|_panic| anyhow::anyhow!("The waiter panicked"))??;
        assert_ne!(found.directory, std::path::absolute(&earlier)?);
        assert_eq!(found.directory, std::path::absolute(&fresh)?);
        assert_eq!(found.health(), recall::Health::Ok);
        Ok(())
    }

    #[test]
    fn wait_selects_the_first_saved_of_several_shows_seen_at_once() -> anyhow::Result<()> {
        let now = SystemTime::now();
        let (earlier, later) = (now - Duration::from_secs(60), now + Duration::from_secs(60));
        for (first_recorded, second_recorded) in [(later, earlier), (earlier, later)] {
            let root = tempfile::tempdir()?;
            save(root.path(), SystemTime::now())?;
            let (found, saved) = wait_for(
                root.path(),
                &[
                    (first_recorded, now),
                    (second_recorded, now + Duration::from_secs(10)),
                ],
            )?;
            assert_eq!(found, saved[0]);
        }
        Ok(())
    }

    #[test]
    fn shows_saved_in_the_same_instant_resolve_to_the_later_recording() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let now = SystemTime::now();
        let minute = Duration::from_secs(60);
        let (found, saved) = wait_for(
            root.path(),
            &[
                (now, now),
                (now + minute, now),
                (now + minute * 2, now + Duration::from_secs(10)),
            ],
        )?;
        assert_eq!(found, saved[1]);
        Ok(())
    }

    #[test]
    fn a_show_removed_before_it_is_opened_does_not_end_the_wait() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let kept = save(root.path(), SystemTime::now())?;
        let saved = saved_at(&kept).context("The kept Show has no save time")?;
        let found = first_saved(
            root.path(),
            vec![
                (None, root.path().join("removed before lookup")),
                (Some(saved), kept.clone()),
                (
                    Some(saved - Duration::from_secs(1)),
                    root.path().join("removed after lookup"),
                ),
            ],
        )?
        .context("The remaining Show was skipped")?;
        assert_eq!(found.directory, std::path::absolute(&kept)?);
        Ok(())
    }

    #[test]
    fn wait_gives_up_without_naming_an_older_show() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        save(root.path(), SystemTime::now())?;
        let error = next_saved_polling(
            root.path(),
            Duration::from_millis(50),
            Duration::from_millis(10),
            thread::sleep,
        )
        .err()
        .context("An older Show satisfied the wait")?;
        assert!(error.to_string().starts_with("No new Show was saved"));
        Ok(())
    }
}
