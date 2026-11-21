use std::{
    collections::HashSet,
    fs,
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
        if let Some(directory) = shows(parent)?
            .into_iter()
            .filter(|directory| !known.contains(directory))
            .map(|directory| (saved_at(&directory), directory))
            .min()
            .map(|(_, directory)| directory)
        {
            return Entry::open(&directory)
                .with_context(|| format!("Cannot open Show {}", directory.display()));
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

fn saved_at(directory: &Path) -> SystemTime {
    fs::symlink_metadata(directory.join(SAVED_AT))
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|metadata| metadata.modified().ok())
        .or_else(|| fs::metadata(directory).ok()?.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH)
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

    fn save(parent: &Path) -> anyhow::Result<PathBuf> {
        save_recorded(parent, SystemTime::now())
    }

    fn save_recorded(parent: &Path, recorded: SystemTime) -> anyhow::Result<PathBuf> {
        let directory = store::fresh_directory(parent, recorded)?;
        let bundle = Demo::new(Typeface::parse(crate::BRAND_FONT_FILE)?).bundle((320, 200))?;
        store::stage(store::managed(&directory, None)?, bundle, None)?.persist()?;
        Ok(directory)
    }

    #[test]
    fn wait_reports_only_a_show_saved_after_it_began() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let earlier = save(root.path())?;
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
        let fresh = save(root.path())?;
        let found = waiter
            .join()
            .map_err(|_panic| anyhow::anyhow!("The waiter panicked"))??;
        assert_ne!(found.directory, std::path::absolute(&earlier)?);
        assert_eq!(found.directory, std::path::absolute(&fresh)?);
        assert_eq!(found.health(), recall::Health::Ok);
        Ok(())
    }

    #[test]
    fn wait_selects_the_earliest_of_several_shows_seen_at_once() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let _older = save(root.path())?;
        let mut first = None;
        let found = next_saved_polling(root.path(), Duration::from_secs(20), POLL, |_| {
            assert!(first.is_none());
            first = Some(save(root.path()).unwrap());
            let earlier = SystemTime::now() - Duration::from_secs(60);
            let _second = save_recorded(root.path(), earlier).unwrap();
        })?;
        assert_eq!(found.directory, first.context("No Show was saved")?);
        Ok(())
    }

    #[test]
    fn wait_gives_up_without_naming_an_older_show() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        save(root.path())?;
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
