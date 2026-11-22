use std::time::Duration;

use anyhow::{Context as _, bail};
use tokio::time::timeout;

use super::*;
use crate::{config::valid_config, theme::Theme};

async fn write_result(progress: SaveProgress) -> anyhow::Result<anyhow::Result<SavedConfig>> {
    let SaveProgress::Writing(result) = progress else {
        bail!("No write started");
    };
    Ok(timeout(Duration::from_secs(5), result.recv()).await??)
}

fn succeeded(progress: SaveProgress) -> anyhow::Result<Box<SavedConfig>> {
    match progress {
        SaveProgress::Finished(SaveOutcome::Saved(saved)) => Ok(saved),
        SaveProgress::Finished(SaveOutcome::Failed(error)) => bail!("The save failed: {error}"),
        SaveProgress::Writing(_) => bail!("The save is still writing"),
    }
}

#[tokio::test]
async fn repeated_saves_replace_the_queued_draft_and_the_newest_lands_last() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let config = valid_config(directory.path())?;
    let draft = |threads| Config {
        threads,
        ..config.clone()
    };
    let mut queue = SaveQueue::new(path.clone(), config.clone());
    let first = queue
        .submit(draft(4))
        .context("The first save was queued")?;
    assert!(
        queue.submit(draft(8)).is_none(),
        "A save started while another was writing"
    );
    assert!(queue.submit(draft(16)).is_none());
    let latest = queue.finish(write_result(first).await?);
    let saved = succeeded(queue.finish(write_result(latest).await?))?;
    assert_eq!(
        saved.validated.threads, 16,
        "Repeated Save replaces the queued draft"
    );
    assert_eq!(Config::read(&path)?.threads, 16);
    assert!(queue.is_idle());
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
    let write = queue
        .submit(edited.clone())
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
        .submit(unwritable.clone())
        .context("The failing save was queued")?;
    let SaveProgress::Finished(SaveOutcome::Failed(_)) = queue.finish(write_result(write).await?)
    else {
        bail!("The failing save did not fail");
    };
    assert!(
        queue.needs_save(&unwritable),
        "A draft whose write failed had nothing to save"
    );
    Ok(())
}
