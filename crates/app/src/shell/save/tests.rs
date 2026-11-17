use std::{sync::mpsc, time::Duration};

use anyhow::{Context as _, bail};
use tokio::time::timeout;

use super::*;
use crate::{config::valid_config, testing::PATIENCE, theme::Theme};

fn durable(config: Config) -> SavedConfig {
    SavedConfig {
        submitted: config.clone(),
        validated: ValidConfig::assume(config),
    }
}

async fn write_result(progress: SaveProgress) -> anyhow::Result<anyhow::Result<SavedConfig>> {
    let SaveProgress::Writing(result) = progress else {
        bail!("No write started");
    };
    Ok(timeout(Duration::from_secs(5), result.recv()).await??)
}

fn succeeded(progress: SaveProgress) -> anyhow::Result<Box<SavedConfig>> {
    match progress {
        SaveProgress::Finished {
            outcome: SaveOutcome::Saved(saved),
            ..
        } => Ok(saved),
        SaveProgress::Finished {
            outcome: SaveOutcome::Failed { error, .. },
            ..
        } => bail!("The save failed: {error}"),
        SaveProgress::Writing(_) => bail!("The save is still writing"),
    }
}

#[tokio::test]
async fn queued_save_applies_only_latest_successful_draft() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let config = valid_config(directory.path())?;
    let epoch = ConfigEpoch::default();
    let draft = |threads| Config {
        threads,
        ..config.clone()
    };
    let mut queue = SaveQueue::new(path.clone(), config.clone());
    let first = queue
        .submit(draft(4), epoch)
        .context("The first save was queued")?;
    assert!(
        queue.submit(draft(8), epoch).is_none(),
        "A save started while another was writing"
    );
    assert!(queue.submit(draft(16), epoch).is_none());
    let latest = queue.finish(write_result(first).await?);
    let SaveProgress::Finished {
        outcome: SaveOutcome::Saved(saved),
        epoch: finished,
    } = queue.finish(write_result(latest).await?)
    else {
        bail!("Latest save did not succeed");
    };
    assert_eq!(
        saved.validated.threads, 16,
        "Repeated Save replaces the queued draft"
    );
    assert_eq!(Config::read(&path)?.threads, 16);
    assert_eq!(finished, epoch);
    assert!(queue.is_idle());
    assert!(queue.kept.is_none());
    Ok(())
}

#[tokio::test]
async fn queued_failure_retains_previous_durable_save_and_reports_latest_error()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let first_epoch = ConfigEpoch::default();
    let queued_epoch = first_epoch.next();
    let mut queue = SaveQueue::new(path, Config::default());
    let draft = Config {
        threads: 4,
        ..valid_config(directory.path())?
    };
    let first = queue
        .submit(draft, first_epoch)
        .context("The first save was queued")?;
    assert!(
        queue.submit(Config::default(), queued_epoch).is_none(),
        "An unconfigured save started while another was writing"
    );
    let queued = queue.finish(write_result(first).await?);
    let SaveProgress::Finished {
        outcome: SaveOutcome::Failed { kept, .. },
        epoch,
    } = queue.finish(write_result(queued).await?)
    else {
        bail!("The queued failure was not reported");
    };
    let kept = kept.context("Previous durable save was lost")?;
    assert_eq!(kept.validated.threads, 4);
    assert_eq!(
        epoch, queued_epoch,
        "A drained queue reports the epoch of its latest request"
    );
    assert!(queue.is_idle());
    assert!(queue.kept.is_none());
    Ok(())
}

#[tokio::test]
async fn a_revert_queued_behind_a_write_reaches_the_file() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let original = valid_config(directory.path())?;
    Config::save_changes(
        &[&original],
        &original,
        &ValidConfig::assume(original.clone()),
        &path,
    )?;
    let iris = Config {
        theme: Theme::Iris,
        ..original.clone()
    };
    let epoch = ConfigEpoch::default();
    let mut queue = SaveQueue::new(path.clone(), original.clone());
    let first = queue
        .submit(iris.clone(), epoch)
        .context("The first save was queued")?;
    assert!(
        queue.needs_save(&original),
        "A revert made while the first write ran had nothing to save"
    );
    assert!(queue.submit(original.clone(), epoch).is_none());
    assert!(!queue.needs_save(&original), "The queued revert had edits");
    assert!(
        queue.needs_save(&iris),
        "Redoing the running write behind a queued revert had nothing to save"
    );
    let revert = queue.finish(write_result(first).await?);
    succeeded(queue.finish(write_result(revert).await?))?;
    assert_eq!(Config::read(&path)?.theme, original.theme);
    Ok(())
}

#[tokio::test]
async fn a_save_queued_behind_a_write_keeps_the_path_spellings_it_did_not_change()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    valid_config(directory.path())?;
    std::fs::write(
        &path,
        r#"{"engine_executable":"engine","model":"model.gguf"}"#,
    )?;

    let opened = Config::read(&path)?;
    let draft = |theme| Config {
        theme,
        ..opened.clone()
    };
    let epoch = ConfigEpoch::default();
    let mut queue = SaveQueue::new(path.clone(), opened.clone());
    let first = queue
        .submit(draft(Theme::Iris), epoch)
        .context("The first save was queued")?;
    assert!(queue.submit(draft(Theme::Chrome), epoch).is_none());
    let queued = queue.finish(write_result(first).await?);
    succeeded(queue.finish(write_result(queued).await?))?;
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(
        saved,
        serde_json::json!({
            "engine_executable": "engine",
            "model": "model.gguf",
            "theme": "chrome",
        })
    );
    Ok(())
}

#[test]
fn save_completion_preserves_new_edits() {
    let submitted = Config {
        threads: 4,
        ..Config::default()
    };
    let mut draft = submitted.clone();
    let mut validated = submitted.clone();
    validated.model = PathBuf::from("/resolved/model.gguf");
    let saved = SavedConfig {
        submitted,
        validated: ValidConfig::assume(validated),
    };
    draft.threads = 8;
    saved.update_draft(&mut draft);
    assert_eq!(draft.threads, 8);
    assert!(draft.model.as_os_str().is_empty());
    draft = saved.submitted.clone();
    saved.update_draft(&mut draft);
    assert_eq!(draft, *saved.validated);
}

#[tokio::test]
async fn a_draft_needs_saving_until_the_file_holds_it_or_a_write_is_making_it() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    valid_config(directory.path())?;

    let opened = Config {
        engine_executable: "engine".into(),
        model: "model.gguf".into(),
        ..Config::default()
    };
    let mut queue = SaveQueue::new(path, opened.clone());
    assert!(
        !queue.needs_save(&opened),
        "An untouched first launch had edits"
    );
    assert!(
        queue.needs_save(&Config::default()),
        "Edits back to the defaults the missing file reads as had nothing to save"
    );
    let edited = Config {
        theme: Theme::Iris,
        ..opened.clone()
    };
    assert!(queue.needs_save(&edited));
    let epoch = ConfigEpoch::default();
    let write = queue
        .submit(edited.clone(), epoch)
        .context("The save was queued")?;
    assert!(
        !queue.needs_save(&edited),
        "A draft being written had edits"
    );
    let saved = succeeded(queue.finish(write_result(write).await?))?;
    assert!(
        !queue.needs_save(&edited),
        "The draft as submitted had edits"
    );
    assert!(
        !queue.needs_save(&saved.validated),
        "The draft with resolved paths had edits"
    );
    assert!(
        queue.needs_save(&opened),
        "A revert the file no longer holds had nothing to save"
    );
    let unwritable = Config {
        model: "missing.gguf".into(),
        ..edited.clone()
    };
    let write = queue
        .submit(unwritable.clone(), epoch)
        .context("The failing save was queued")?;
    assert!(
        queue.needs_save(&edited),
        "A revert made while a later write ran had nothing to save"
    );
    let SaveProgress::Finished {
        outcome: SaveOutcome::Failed { .. },
        ..
    } = queue.finish(write_result(write).await?)
    else {
        bail!("The failing save did not fail");
    };
    assert!(
        queue.needs_save(&unwritable),
        "A draft whose write failed had nothing to save"
    );
    Ok(())
}

#[test]
fn save_worker_returns_before_storage_and_joins_owned_work() -> anyhow::Result<()> {
    let (entered, started) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let work = SaveWork::spawn(move || {
        entered.send(())?;
        released.recv()?;
        Ok(durable(Config::default()))
    })?;
    started.recv_timeout(PATIENCE)?;
    assert!(work.result.try_recv().is_err());
    let (joining, joined) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let cleanup = std::thread::spawn(move || {
        joining.send(()).unwrap();
        drop(work);
        finished.send(()).unwrap();
    });
    joined.recv_timeout(PATIENCE)?;
    assert!(completion.recv_timeout(Duration::from_millis(10)).is_err());
    release.send(())?;
    completion.recv_timeout(PATIENCE)?;
    cleanup.join().unwrap();
    Ok(())
}

#[test]
fn flush_finishes_the_running_write_before_a_revert_queued_behind_it() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let original = valid_config(directory.path())?;
    let first = Config {
        theme: Theme::Iris,
        ..original.clone()
    };
    let (entered, started) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (base, written, first_path) = (original.clone(), first.clone(), path.clone());
    let running = SaveWork::spawn(move || {
        entered.send(())?;
        released.recv()?;
        Config::save_changes(
            &[&base],
            &written,
            &ValidConfig::assume(written.clone()),
            &first_path,
        )?;
        Ok(durable(written))
    })?;
    started.recv_timeout(PATIENCE)?;
    let mut queue = SaveQueue {
        writing: Some(Running {
            work: running,
            draft: first,
            queued: Some(original.clone()),
        }),
        ..SaveQueue::new(path.clone(), original.clone())
    };
    let cleanup = std::thread::spawn(move || queue.flush());
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !path.exists(),
        "The queued save started before the running write finished"
    );
    release.send(())?;
    cleanup.join().unwrap();
    assert_eq!(Config::read(&path)?.theme, original.theme);
    Ok(())
}
