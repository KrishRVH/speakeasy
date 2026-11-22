use std::{fs, path::Path};

use serde_json::json;
use speakeasy_platform::InputSender;
use speakeasy_show::{Image, Manifest};

use super::*;
use crate::testing::PATIENCE;

fn no_wsl() -> Lookup {
    Lookup::spawn(|| None, PATIENCE)
}

#[tokio::test]
async fn a_new_recording_revokes_a_blocked_copy_and_no_copy_overtakes_it() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("summary.md"), "Summary")?;
    let (sender, _events) = async_channel::bounded(1);
    let input = InputSender::new(sender);
    let mut copies = Copies::new(no_wsl());
    let (reached, acquired) = async_channel::bounded(1);
    let (release, waiting) = std::sync::mpsc::channel();
    let (published, written) = async_channel::bounded(1);
    let receipt = copies.start(
        Request::Summary(root.path().to_owned()),
        input.idle(),
        move |_, authorized| {
            reached.try_send(())?;
            waiting.recv_timeout(PATIENCE)?;
            let allowed = authorized();
            published.try_send(allowed)?;
            Ok(allowed)
        },
    )?;
    tokio::time::timeout(PATIENCE, acquired.recv()).await??;
    assert!(input.begin().unwrap().commit());
    let replacement = copies.start(Request::Summary(root.path().to_owned()), None, |_, _| {
        Ok(true)
    });
    release.send(())?;
    tokio::time::timeout(PATIENCE, copies.drained()).await?;
    assert!(
        replacement.is_err(),
        "A second copy started while the first still held the clipboard"
    );
    assert!(
        !written.recv().await?,
        "A copy wrote the clipboard after newer dictation began"
    );
    assert!(receipt.await.is_err());
    Ok(())
}

#[tokio::test]
async fn a_missing_saved_show_does_not_copy_another_show_or_report_success() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let mut copies = Copies::new(no_wsl());
    let receipt = copies.start(
        Request::Saved(super::super::SavedShow::assume(root.path().join("missing"))),
        None,
        |_, _| anyhow::bail!("A missing Show touched the clipboard"),
    )?;
    assert!(matches!(
        tokio::time::timeout(PATIENCE, receipt)
            .await?
            .map_err(anyhow::Error::msg)?,
        Outcome::Missing
    ));
    copies.drained().await;
    Ok(())
}

#[tokio::test]
async fn native_failure_reaches_the_requester_and_the_next_copy_can_run() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("summary.md"), "Summary")?;
    let mut copies = Copies::new(no_wsl());
    let receipt = copies.start(Request::Summary(root.path().to_owned()), None, |_, _| {
        anyhow::bail!("Clipboard is held")
    })?;
    let error = tokio::time::timeout(PATIENCE, receipt)
        .await?
        .err()
        .unwrap();
    assert!(error.contains("Clipboard is held"));
    copies.drained().await;
    let receipt = copies.start(
        Request::Summary(root.path().to_owned()),
        None,
        |_, authorized| Ok(authorized()),
    )?;
    assert!(matches!(
        tokio::time::timeout(PATIENCE, receipt)
            .await?
            .map_err(anyhow::Error::msg)?,
        Outcome::Copied
    ));
    copies.drained().await;
    Ok(())
}

fn saved_show(root: &Path) -> anyhow::Result<PathBuf> {
    let directory = root.join("20261101T093000Z");
    fs::create_dir_all(directory.join("frames"))?;
    let keyframes: Vec<_> = (1..=7)
        .zip((100..=700).step_by(100))
        .map(|(index, at)| {
            json!({"index": index, "at": at, "reason": "middle", "event": "fixture", "marks": [0, 0]})
        })
        .collect();
    let manifest = serde_json::to_vec(&json!({
        "version": 3, "duration": 1000, "surface": {"width": 2, "height": 2},
        "strokes": [], "pins": [], "actions": [], "narration": {"text": "Narration"},
        "marks": [], "keyframes": keyframes
    }))?;
    let png = Image::from_bgra(2, 2, [40, 80, 120, 255].repeat(4))?.encode_png()?;
    for name in Manifest::parse(&manifest)?.files() {
        fs::write(directory.join(name), &png)?;
    }
    fs::write(directory.join(Manifest::FILE), &manifest)?;
    fs::write(directory.join(Bundle::SUMMARY), "Summary")?;
    Ok(directory)
}

#[test]
fn an_agent_prompt_points_at_every_sheet_in_order() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = saved_show(root.path())?;
    let prompt = agent_prompt(&directory, || None)?.context("An existing Show has a prompt")?;
    let listed: Vec<&str> = prompt
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .collect();
    let expected = ["contact-sheet.png", "contact-sheet-02.png"]
        .map(|name| directory.join(name).display().to_string());
    assert_eq!(
        listed, expected,
        "The prompt lost or reordered a contact sheet"
    );
    assert!(prompt.contains(&directory.join(Bundle::SUMMARY).display().to_string()));
    assert!(prompt.contains("\nShow ID: 20261101T093000Z\n"), "{prompt}");
    assert!(!prompt.contains("WSL"), "{prompt}");
    assert!(
        Request::Agent(root.path().join("missing"))
            .prepare(&no_wsl())?
            .is_none(),
        "A missing Show produced a prompt for another folder"
    );
    Ok(())
}

#[test]
fn a_windows_prompt_also_names_the_folder_as_wsl_sees_it() -> anyhow::Result<()> {
    let directory = Path::new("C:/Users/ada/AppData/Local/speakeasy/shows/20261117T093000Z");
    let wsl = Wsl::configured("");
    let location = Location::managed(directory, Some(&wsl)).context("A named folder")?;
    let prompt = prompt(
        location,
        &[directory.join("contact-sheet.png")],
        &directory.join(Bundle::SUMMARY),
    )?;
    let lines: Vec<&str> = prompt.lines().collect();
    assert_eq!(
        lines[2..5],
        [
            "Show ID: 20261117T093000Z",
            "Folder: C:/Users/ada/AppData/Local/speakeasy/shows/20261117T093000Z",
            "Folder from WSL: /mnt/c/Users/ada/AppData/Local/speakeasy/shows/20261117T093000Z",
        ]
    );
    assert!(
        prompt.contains("An agent with Speakeasy's MCP tools can open this Show by its ID."),
        "{prompt}"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
#[test]
fn send_to_agent_names_the_folder_the_wsl_lookup_maps() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = saved_show(root.path())?;
    let wsl = Lookup::spawn(|| Some(Wsl::configured("")), PATIENCE);
    let prompt =
        agent_prompt(&directory, || wsl.found())?.context("An existing Show has a prompt")?;
    let mapped = Wsl::configured("")
        .path(&std::path::absolute(&directory)?)
        .context("A temporary folder on a lettered drive maps into WSL")?;
    assert!(
        prompt.contains(&format!("\nFolder from WSL: {mapped}\n")),
        "{prompt}"
    );
    Ok(())
}
