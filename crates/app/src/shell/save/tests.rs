//! Drives the save queue through writes to temporary settings files: the latest queued draft lands,
//! a failure keeps the last durable save, and dropping or flushing waits for the running write.

use std::{sync::mpsc, time::Duration};

use anyhow::{Context as _, bail};
use tokio::time::timeout;

use super::*;
use crate::{config::valid_config, theme::Theme};

fn durable(config: Config) -> SavedConfig {
    SavedConfig {
        submitted: config.clone(),
        validated: ValidConfig::assume(config),
    }
}

/// Waits for the result of the write `progress` started, for `SaveQueue::finish`.
async fn write_result(progress: SaveProgress) -> anyhow::Result<anyhow::Result<SavedConfig>> {
    let SaveProgress::Writing(result) = progress else {
        bail!("No write started");
    };
    Ok(timeout(Duration::from_secs(5), result.recv()).await??)
}

#[tokio::test]
async fn queued_save_applies_only_latest_successful_draft() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let config = valid_config(directory.path())?;
    let epoch = ConfigEpoch::default();
    let request = |threads| SaveRequest {
        draft: Config {
            threads,
            ..config.clone()
        },
        epoch,
        path: path.clone(),
    };
    let mut queue = SaveQueue::new(config.clone());
    let first = queue
        .submit(request(4))
        .context("The first save was queued")?;
    assert!(
        queue.submit(request(8)).is_none(),
        "A save started while another was writing"
    );
    assert!(queue.submit(request(16)).is_none());
    let latest = queue.finish(write_result(first).await?);
    let SaveProgress::Finished {
        saved,
        error,
        epoch: finished,
    } = queue.finish(write_result(latest).await?)
    else {
        bail!("Latest save did not finish");
    };
    let saved = saved.context("Successful save was lost")?;
    assert_eq!(
        saved.validated.threads, 16,
        "Repeated Save replaces the queued draft"
    );
    assert_eq!(Config::read(&path)?.threads, 16);
    assert_eq!(finished, epoch);
    assert!(error.is_none());
    assert!(queue.is_idle());
    assert!(queue.saved.is_none());
    Ok(())
}

#[tokio::test]
async fn queued_failure_retains_previous_durable_save_and_reports_latest_error()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let first_epoch = ConfigEpoch::default();
    let queued_epoch = first_epoch.next();
    let mut queue = SaveQueue::new(Config::default());
    let first = queue
        .submit(SaveRequest {
            draft: Config {
                threads: 4,
                ..valid_config(directory.path())?
            },
            epoch: first_epoch,
            path: path.clone(),
        })
        .context("The first save was queued")?;
    let unconfigured = SaveRequest {
        draft: Config::default(),
        epoch: queued_epoch,
        path,
    };
    assert!(queue.submit(unconfigured).is_none());
    let queued = queue.finish(write_result(first).await?);
    let SaveProgress::Finished {
        saved,
        error,
        epoch,
    } = queue.finish(write_result(queued).await?)
    else {
        bail!("Failed save did not finish");
    };
    let saved = saved.context("Previous durable save was lost")?;
    assert_eq!(saved.validated.threads, 4);
    assert!(error.is_some(), "The queued failure was not reported");
    assert_eq!(
        epoch, queued_epoch,
        "A drained queue reports the epoch of its latest request"
    );
    assert!(queue.is_idle());
    assert!(queue.saved.is_none());
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
    let request = |theme| SaveRequest {
        draft: Config {
            theme,
            ..original.clone()
        },
        epoch: ConfigEpoch::default(),
        path: path.clone(),
    };
    let mut queue = SaveQueue::new(original.clone());
    let first = queue
        .submit(request(Theme::Iris))
        .context("The first save was queued")?;
    assert!(queue.submit(request(original.theme)).is_none());
    let revert = queue.finish(write_result(first).await?);
    let SaveProgress::Finished { error, .. } = queue.finish(write_result(revert).await?) else {
        bail!("The revert did not finish");
    };
    assert!(error.is_none());
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
    // The draft keeps relative paths, as after a launch whose settings did not validate.
    let draft = Config::read(&path)?;
    let request = |theme| SaveRequest {
        draft: Config {
            theme,
            ..draft.clone()
        },
        epoch: ConfigEpoch::default(),
        path: path.clone(),
    };
    let mut queue = SaveQueue::new(draft.clone());
    let first = queue
        .submit(request(Theme::Iris))
        .context("The first save was queued")?;
    assert!(queue.submit(request(Theme::Chrome)).is_none());
    let queued = queue.finish(write_result(first).await?);
    let SaveProgress::Finished { error, .. } = queue.finish(write_result(queued).await?) else {
        bail!("The queued save did not finish");
    };
    assert!(error.is_none());
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
    assert!(!saved.update_draft(&mut draft));
    assert_eq!(draft.threads, 8);
    assert!(draft.model.as_os_str().is_empty());
    draft = saved.submitted.clone();
    assert!(saved.update_draft(&mut draft));
    assert!(draft == *saved.validated);
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
    started.recv_timeout(Duration::from_secs(2))?;
    assert!(work.result.try_recv().is_err());
    let (joining, joined) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let cleanup = std::thread::spawn(move || {
        joining.send(()).unwrap();
        drop(work);
        finished.send(()).unwrap();
    });
    joined.recv_timeout(Duration::from_secs(2))?;
    assert!(completion.recv_timeout(Duration::from_millis(10)).is_err());
    release.send(())?;
    completion.recv_timeout(Duration::from_secs(2))?;
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
    let (base, first_path) = (original.clone(), path.clone());
    let running = SaveWork::spawn(move || {
        entered.send(())?;
        released.recv()?;
        Config::save_changes(
            &[&base],
            &first,
            &ValidConfig::assume(first.clone()),
            &first_path,
        )?;
        Ok(durable(first))
    })?;
    started.recv_timeout(Duration::from_secs(2))?;
    let mut queue = SaveQueue {
        writing: Some(running),
        pending: Some(SaveRequest {
            draft: original.clone(),
            epoch: ConfigEpoch::default(),
            path: path.clone(),
        }),
        ..SaveQueue::new(original.clone())
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
