use std::{
    process::Stdio,
    sync::mpsc::{self, RecvTimeoutError},
};

use anyhow::{Context as _, bail, ensure};
use tokio::time::timeout;

use super::*;
use crate::{
    local_speech::LocalSpeech,
    runtime::ports::Speech,
    testing::{PATIENCE, still_pending},
};

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

fn setup_held_in_cleanup() -> anyhow::Result<(Setup, mpsc::Receiver<()>, mpsc::Sender<()>)> {
    let (entered, cleaning) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let setup = Setup::spawn(move |_, cancelled| {
        let Err(_closed) = cancelled.recv_blocking();
        entered.send(()).unwrap();

        released.recv_timeout(PATIENCE).unwrap();
        None
    })?;
    Ok((setup, cleaning, release))
}

#[tokio::test]
async fn cancelled_setup_remains_owned_while_cleanup_is_pending() -> anyhow::Result<()> {
    let (setup, cleaning, release) = setup_held_in_cleanup()?;
    setup.request_stop();
    let stopped = setup.stopped();
    cleaning.recv_timeout(PATIENCE)?;
    assert!(
        still_pending(setup.stopped()).await,
        "A cancelled setup reported stopping before its cleanup finished"
    );
    assert!(!setup.is_finished());
    release.send(())?;
    timeout(PATIENCE, stopped).await?;
    Ok(())
}

#[test]
fn a_full_disk_asks_for_space_and_other_file_failures_for_access() {
    let message =
        |kind: io::ErrorKind| format!("{:#}", file_error(Path::new("models"))(kind.into()));
    let full = message(io::ErrorKind::StorageFull);
    assert!(
        full.starts_with("Setup cannot use models: ")
            && full.ends_with(". Free up disk space, then try again."),
        "{full}"
    );
    let denied = message(io::ErrorKind::PermissionDenied);
    assert!(
        denied.contains("check that you can write there"),
        "{denied}"
    );
}

#[test]
fn setup_drop_joins_cancelled_cleanup_before_returning() -> anyhow::Result<()> {
    let (setup, cleaning, release) = setup_held_in_cleanup()?;
    let (finished, joined) = mpsc::channel();
    let quit = std::thread::spawn(move || {
        drop(setup);
        finished.send(()).unwrap();
    });
    cleaning.recv_timeout(PATIENCE)?;
    assert_eq!(
        joined.recv_timeout(Duration::from_millis(30)),
        Err(RecvTimeoutError::Timeout),
        "Dropping a setup returned before its cleanup finished"
    );
    release.send(())?;
    joined.recv_timeout(PATIENCE)?;
    quit.join().unwrap();
    Ok(())
}

#[tokio::test]
async fn setup_drop_reaps_its_child_and_releases_the_directory() -> anyhow::Result<()> {
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
    ensure!(
        !is_locked(&root.join("setup.lock"))?,
        "Dropping setup kept the directory locked"
    );
    Ok(())
}

#[test]
#[ignore = "Subprocess helper for the setup cancellation tests; does nothing unless fixture_child spawns it"]
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
    .context("The setup child did not start")?;
    let lock = root.join("child.lock");
    ensure!(is_locked(&lock)?, "The setup child did not take its lock");
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
async fn cancellation_stops_the_child_before_another_setup_owns_the_directory() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let root = directory.path().to_path_buf();
    let partial = b"partial model";
    fs::write(root.join("model.gguf.part"), partial)?;
    let (progress, _) = watch::channel(CHECKING);
    let (cancel, cancelled) = async_channel::bounded::<Infallible>(1);
    let first_root = root.clone();
    let first = tokio::spawn(async move {
        Installation::run(&first_root, &progress, &cancelled, async |installation| {
            installation.output(&mut fixture_child(&first_root)?).await
        })
        .await
    });
    let child_lock = started_child(&root).await?;

    let (progress, mut updates) = watch::channel(CHECKING);
    let (_second_cancel, second_cancelled) = async_channel::bounded::<Infallible>(1);
    let second_root = root.clone();
    let second = tokio::spawn(async move {
        Installation::run(&second_root, &progress, &second_cancelled, async |_| {
            ensure!(
                !is_locked(&child_lock)?,
                "A retry entered the directory while the cancelled child could still write there"
            );
            ensure!(
                fs::read(second_root.join("model.gguf.part"))? == partial,
                "A retry changed the partial download"
            );
            Ok(())
        })
        .await
    });
    timeout(Duration::from_secs(5), updates.changed()).await??;
    assert!(matches!(
        *updates.borrow(),
        Progress::Step("Waiting for another setup to finish")
    ));
    assert!(!second.is_finished());
    drop(cancel);
    assert!(timeout(Duration::from_secs(5), first).await??.is_none());
    timeout(Duration::from_secs(5), second)
        .await??
        .context("Retry was cancelled")??;
    Ok(())
}

#[tokio::test]
async fn cancellation_while_waiting_for_setup_leaves_the_owner_locked() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let (progress, _) = watch::channel(CHECKING);
    let _owner = Installation::acquire(directory.path(), &progress).await?;
    let (progress, mut updates) = watch::channel(CHECKING);
    let (cancel, cancelled) = async_channel::bounded::<Infallible>(1);
    let root = directory.path().to_path_buf();
    let waiting = tokio::spawn(async move {
        Installation::run(&root, &progress, &cancelled, async |_| {
            bail!("A waiting setup entered the owned directory")
        })
        .await
    });
    timeout(Duration::from_secs(5), updates.changed()).await??;
    drop(cancel);
    let result: Option<anyhow::Result<()>> = timeout(Duration::from_secs(5), waiting).await??;
    assert!(result.is_none());
    ensure!(
        is_locked(&directory.path().join("setup.lock"))?,
        "A cancelled waiter released the owner's lock"
    );
    Ok(())
}

#[tokio::test]
async fn setup_names_the_directory_it_cannot_prepare_and_what_to_do() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("file"), b"")?;
    let beneath_a_file = directory.path().join("file").join("speakeasy");
    let lock_is_a_directory = directory.path().join("speakeasy");
    fs::create_dir_all(lock_is_a_directory.join("setup.lock"))?;
    let (progress, _) = watch::channel(CHECKING);
    for root in [beneath_a_file, lock_is_a_directory] {
        let failure = Installation::acquire(&root, &progress)
            .await
            .err()
            .context("Setup prepared a directory it cannot lock")?
            .to_string();
        assert!(
            failure.starts_with(&format!("Setup cannot use {}: ", root.display()))
                && failure.ends_with(", then try again."),
            "{failure}"
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "Downloads about 0.8 GB; requires SPEAKEASY_FIXTURE_WAV (whisper.cpp samples/jfk.wav)"]
async fn install_chooses_an_engine_that_recognizes_fixture_speech() -> anyhow::Result<()> {
    let wav = fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
    let root = tempfile::tempdir()?;
    let (progress, _) = watch::channel(CHECKING);
    let (_cancel, cancelled) = async_channel::bounded::<Infallible>(1);
    let installed = Installation::run(root.path(), &progress, &cancelled, async |installation| {
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
        "Fixture recognition did not contain the expected phrase"
    );
    Ok(())
}
