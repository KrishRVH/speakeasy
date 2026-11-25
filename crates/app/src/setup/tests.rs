use std::{
    fs::{self, File},
    process::Stdio,
};

use anyhow::{Context as _, ensure};
use tokio::time::timeout;

use super::*;
use crate::{local_speech::LocalSpeech, runtime::ports::Speech};

fn installed(choice: Model, use_gpu: bool) -> Installed {
    Installed {
        choice,
        engine_executable: "engine".into(),
        model: choice.file_name().into(),
        use_gpu,
    }
}

#[test]
fn each_managed_model_downloads_under_the_name_that_identifies_it() {
    for model in Model::ALL {
        let (download, _) = source(model);
        assert_eq!(
            Model::at(Path::new(download.file_name())),
            Some(model),
            "{model:?}"
        );
    }
}

#[test]
fn model_installation_preserves_language_gpu_preference_and_unrelated_edits() {
    for choice in Model::ALL {
        let mut config = Config {
            language: "ja-JP".into(),
            use_gpu: false,
            threads: 8,
            ..Config::default()
        };
        installed(choice, true).apply_to(&mut config);
        assert_eq!(config.language, "ja-JP");
        assert!(!config.use_gpu);
        assert_eq!(config.threads, 8);
    }
}

#[test]
fn a_nemotron_install_gives_a_bare_language_its_region() {
    let mut config = Config {
        language: "en".into(),
        ..Config::default()
    };
    installed(Model::Nemotron, true).apply_to(&mut config);
    assert_eq!(config.language, "en-US");
}

#[test]
fn an_install_without_gpu_support_turns_gpu_off() {
    let mut config = Config {
        use_gpu: true,
        ..Config::default()
    };
    installed(Model::Parakeet, false).apply_to(&mut config);
    assert!(!config.use_gpu);
}

#[test]
fn a_full_disk_asks_for_space_and_other_file_failures_for_access() {
    let message =
        |kind: io::ErrorKind| format!("{:#}", file_error(Path::new("models"))(kind.into()));
    let full = message(io::ErrorKind::StorageFull);
    assert!(
        full.starts_with("Setup can't use models: ")
            && full.ends_with(". Free up disk space, then try again."),
        "{full}"
    );
    let denied = message(io::ErrorKind::PermissionDenied);
    assert!(
        denied.contains("check that you can write there"),
        "{denied}"
    );
}

#[tokio::test]
async fn setup_drop_reaps_its_child() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().to_path_buf();
    let setup = Setup::start_in(root.clone(), async |root, _, installation| {
        installation.output(&mut fixture_child(root)?).await?;
        Ok(installed(Model::Parakeet, false))
    })?;
    let child_lock = started_child(&root).await?;
    drop(setup);
    ensure!(
        !is_locked(&child_lock)?,
        "Dropping setup left its child running"
    );
    Ok(())
}

#[test]
#[ignore = "Subprocess helper for the setup drop test; does nothing unless fixture_child spawns it"]
fn setup_child_fixture() -> anyhow::Result<()> {
    let Some(root) = std::env::var_os("SPEAKEASY_SETUP_TEST_ROOT").map(PathBuf::from) else {
        return Ok(());
    };
    let lock = File::create(root.join("child.lock"))?;
    lock.lock()?;
    fs::write(root.join("child.ready"), b"")?;
    std::thread::sleep(Duration::from_secs(60));
    Ok(())
}

fn fixture_child(root: &Path) -> anyhow::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);

    command
        .args(["--exact", "setup::tests::setup_child_fixture", "--ignored"])
        .env("SPEAKEASY_SETUP_TEST_ROOT", root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

async fn started_child(root: &Path) -> anyhow::Result<PathBuf> {
    timeout(Duration::from_secs(10), async {
        while !root.join("child.ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("The setup child didn't start")?;
    let lock = root.join("child.lock");
    ensure!(is_locked(&lock)?, "The setup child didn't take its lock");
    Ok(lock)
}

fn is_locked(path: &Path) -> anyhow::Result<bool> {
    match File::open(path)?.try_lock() {
        Ok(()) => Ok(false),
        Err(fs::TryLockError::WouldBlock) => Ok(true),
        Err(fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

#[tokio::test]
#[ignore = "Downloads about 0.8 GB; requires SPEAKEASY_FIXTURE_WAV (whisper.cpp samples/jfk.wav)"]
async fn install_chooses_an_engine_that_recognizes_fixture_speech() -> anyhow::Result<()> {
    let wav = fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
    let root = tempfile::tempdir()?;
    let (progress, _) = watch::channel(CHECKING);
    let (_cancel, cancelled) = async_channel::bounded::<Infallible>(1);
    let installed = Installation::run(&cancelled, async |installation| {
        install(root.path(), &progress, installation, Model::Parakeet).await
    })
    .await
    .context("Setup was cancelled")??;
    eprintln!(
        "Setup chose {} with GPU {}",
        installed.engine_executable.display(),
        installed.use_gpu
    );
    let mut config = Config::default();
    installed.apply_to(&mut config);
    let (_cancel, cancelled) = watch::channel(false);
    let mut worker = LocalSpeech::start(config, cancelled).await?;
    let result = worker
        .transcribe(wav, "en", &crate::recognition_terms::Terms::default())
        .await;
    worker.stop().await;

    assert!(
        result?.to_lowercase().contains("ask not what your country"),
        "Fixture recognition didn't contain the expected phrase"
    );
    Ok(())
}
