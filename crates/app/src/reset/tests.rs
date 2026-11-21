use std::{fs, path::PathBuf};

use anyhow::Context as _;

use super::*;

fn fixture(root: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    let settings = root.join("config").join("settings.json");
    let data = root.join("data");
    fs::create_dir_all(settings.parent().context("settings has a folder")?)?;
    fs::write(&settings, r#"{"engine": "parakeet"}"#)?;
    fs::write(settings.with_added_extension("onboarded"), [])?;
    for file in [
        "engines/nemo-speech-cuda/nemo-speech.exe",
        "models/0123abcd/parakeet-tdt-0.6b-v3.q8_0.gguf",
        "downloads/nemo-speech.tar.gz.part",
        "updates/release-0123abcd/speakeasy-windows-x64-setup.exe",
        "shows/20261020T143205Z/summary.md",
        "shows/20261020T143205Z/.pinned",
    ] {
        let path = data.join(file);
        fs::create_dir_all(path.parent().context("fixture has a folder")?)?;
        fs::write(path, "PUBLIC_FIXTURE")?;
    }
    Ok((settings, data))
}

#[test]
fn a_purge_forgets_settings_and_downloads_but_keeps_every_show() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (settings, data) = fixture(root.path())?;
    purge(&settings, &data)?;
    assert!(!settings.exists(), "The settings survived the purge");
    for folder in DOWNLOADS {
        assert!(!data.join(folder).exists(), "{folder} survived the purge");
    }
    assert!(
        data.join("shows/20261020T143205Z/summary.md").is_file()
            && data.join("shows/20261020T143205Z/.pinned").is_file(),
        "The purge removed a Show"
    );
    assert!(settings.with_added_extension("onboarded").is_file());
    purge(&settings, &data)?;
    Ok(())
}

#[test]
fn a_failed_purge_keeps_the_settings() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (settings, data) = fixture(root.path())?;
    fs::remove_dir_all(data.join("models"))?;
    fs::write(data.join("models"), "PUBLIC_FIXTURE")?;
    assert!(purge(&settings, &data).is_err());
    assert!(
        settings.is_file(),
        "A purge that kept a download still deleted the settings"
    );
    Ok(())
}
