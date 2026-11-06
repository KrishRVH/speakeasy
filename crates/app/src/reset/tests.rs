use std::{
    fs::{self, File},
    path::PathBuf,
};

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
        "updates/Speakeasy-0.8.0.msi",
        "shows/20261020T143205Z/summary.md",
        "shows/20261020T143205Z/.pinned",
    ] {
        let path = data.join(file);
        fs::create_dir_all(path.parent().context("fixture has a folder")?)?;
        fs::write(path, "PUBLIC_FIXTURE")?;
    }
    fs::write(data.join("setup.lock"), [])?;
    Ok((settings, data))
}

#[test]
fn a_purge_forgets_settings_and_downloads_but_keeps_every_show() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (settings, data) = fixture(root.path())?;
    purge(&settings, &data)?;
    assert!(!settings.exists(), "Unreadable settings survived the purge");
    for folder in ["engines", "models", "downloads", "updates"] {
        assert!(!data.join(folder).exists(), "{folder} survived the purge");
    }
    assert!(
        data.join("shows/20261020T143205Z/summary.md").is_file()
            && data.join("shows/20261020T143205Z/.pinned").is_file(),
        "The purge removed a Show"
    );
    assert!(settings.with_added_extension("onboarded").is_file());
    assert!(data.join("setup.lock").is_file());
    purge(&settings, &data)?;
    Ok(())
}

#[test]
fn a_purge_refuses_while_another_copy_sets_up_or_prepares_an_update() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (settings, data) = fixture(root.path())?;
    for lock in [data.join("setup.lock"), data.join("updates.lock")] {
        let held = File::create(&lock)?;
        held.try_lock()?;
        assert!(
            purge(&settings, &data).is_err(),
            "{lock:?} did not stop the purge"
        );
        assert!(
            settings.is_file() && data.join("models").is_dir(),
            "A refused purge removed files"
        );
        drop(held);
    }
    purge(&settings, &data)
}

#[test]
fn a_purge_keeps_both_leases_through_deletion_even_without_a_cache() -> anyhow::Result<()> {
    for cache_exists in [true, false] {
        let root = tempfile::tempdir()?;
        let (settings, data) = fixture(root.path())?;
        if !cache_exists {
            fs::remove_dir_all(data.join("updates"))?;
        }
        purge_with(&settings, &data, |path| {
            remove(path)?;
            for name in ["setup.lock", "updates.lock"] {
                let contender = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(data.join(name))?;
                assert!(matches!(
                    contender.try_lock(),
                    Err(TryLockError::WouldBlock)
                ));
            }
            assert!(settings.is_file());
            Ok(())
        })?;
        for name in ["setup.lock", "updates.lock"] {
            let available = OpenOptions::new()
                .read(true)
                .write(true)
                .open(data.join(name))?;
            available.try_lock()?;
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_failed_purge_keeps_the_settings() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir()?;
    let (settings, data) = fixture(root.path())?;
    let models = data.join("models");
    fs::set_permissions(&models, fs::Permissions::from_mode(0o555))?;
    let purged = purge(&settings, &data);
    fs::set_permissions(&models, fs::Permissions::from_mode(0o755))?;
    assert!(purged.is_err());
    assert!(
        settings.is_file(),
        "A purge that kept a download still deleted the settings"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_linked_folder_loses_only_its_link() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let (settings, data) = fixture(root.path())?;
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere)?;
    fs::write(elsewhere.join("keep.txt"), "PUBLIC_FIXTURE")?;
    fs::remove_dir_all(data.join("models"))?;
    std::os::unix::fs::symlink(&elsewhere, data.join("models"))?;
    purge(&settings, &data)?;
    assert!(!data.join("models").exists());
    assert!(
        elsewhere.join("keep.txt").is_file(),
        "The purge followed a link out of Speakeasy's folder"
    );
    Ok(())
}
