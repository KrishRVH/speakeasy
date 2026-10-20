//! Drives setup cancellation, the directory lock and the owned child through a test-binary
//! fixture process, and installs and recognizes a public recording in an opt-in run.

use std::process::Stdio;

use anyhow::Context as _;
use tokio::time::timeout;

use super::*;
use crate::{local_speech::LocalSpeech, runtime::ports::Speech};

#[tokio::test]
async fn cancelled_setup_remains_owned_while_cleanup_is_pending() -> anyhow::Result<()> {
    let (entered, cleaning) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let setup = Setup::spawn(move |_, cancelled| {
        assert!(cancelled.recv_blocking().is_err());
        entered.send(()).unwrap();
        // A timeout, so a failing test cannot strand this owned thread.
        released.recv_timeout(Duration::from_secs(2)).unwrap();
        None
    })?;
    setup.request_stop();
    let stopped = setup.stopped();
    cleaning.recv_timeout(Duration::from_secs(2))?;
    assert!(
        timeout(Duration::from_millis(30), setup.stopped())
            .await
            .is_err(),
        "A cancelled setup reported stopping before its cleanup finished"
    );
    assert!(!setup.is_finished());
    release.send(())?;
    timeout(Duration::from_secs(2), stopped).await?;
    Ok(())
}

#[test]
fn setup_drop_joins_cancelled_cleanup_before_returning() -> anyhow::Result<()> {
    let (entered, cleaning) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let setup = Setup::spawn(move |_, cancelled| {
        assert!(cancelled.recv_blocking().is_err());
        entered.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(2)).unwrap();
        None
    })?;
    let (finished, joined) = std::sync::mpsc::channel();
    let quit = std::thread::spawn(move || {
        drop(setup);
        finished.send(()).unwrap();
    });
    cleaning.recv_timeout(Duration::from_secs(2))?;
    assert!(joined.recv_timeout(Duration::from_millis(30)).is_err());
    release.send(())?;
    joined.recv_timeout(Duration::from_secs(2))?;
    quit.join().unwrap();
    Ok(())
}

#[test]
fn setup_drop_reaps_its_child_before_releasing_the_directory() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().to_path_buf();
    let installation_root = root.clone();
    let setup = Setup::spawn(move |progress, cancelled| {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => return Some(Err(error.into())),
        };
        runtime.block_on(Installation::run(
            &installation_root,
            &progress,
            &cancelled,
            async |installation| {
                installation
                    .output(&mut fixture_child(&installation_root)?)
                    .await?;
                Ok(Installed {
                    engine_executable: PathBuf::new(),
                    model: PathBuf::new(),
                    use_gpu: false,
                })
            },
        ))
    })?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !root.join("child.ready").exists() {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "Setup child did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let child_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("child.lock"))?;
    assert!(matches!(
        child_lock.try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    drop(setup);
    child_lock.try_lock()?;
    let setup_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("setup.lock"))?;
    setup_lock.try_lock()?;
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
    // No `kill_on_drop`: only the owner's explicit cleanup may stop this child.
    command
        .args(["--exact", "setup::tests::setup_child_fixture", "--ignored"])
        .env("SPEAKEASY_SETUP_TEST_ROOT", root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

#[tokio::test]
async fn cancellation_stops_the_child_before_another_setup_owns_the_directory() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let root = directory.path().to_path_buf();
    let partial = b"partial model";
    fs::write(root.join("model.gguf.part"), partial)?;
    let (progress, _) = watch::channel(Progress::default());
    let (cancel, cancelled) = async_channel::bounded::<Infallible>(1);
    let first_root = root.clone();
    let first = tokio::spawn(async move {
        Installation::run(&first_root, &progress, &cancelled, async |installation| {
            installation.output(&mut fixture_child(&first_root)?).await
        })
        .await
    });
    timeout(Duration::from_secs(10), async {
        while !root.join("child.ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let child_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("child.lock"))?;
    assert!(matches!(
        child_lock.try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));

    let (progress, mut updates) = watch::channel(Progress::default());
    let (_second_cancel, second_cancelled) = async_channel::bounded::<Infallible>(1);
    let second_root = root.clone();
    let second = tokio::spawn(async move {
        Installation::run(&second_root, &progress, &second_cancelled, async |_| {
            child_lock.try_lock().context(
                "A retry entered the directory while the cancelled child could still write there",
            )?;
            assert_eq!(fs::read(second_root.join("model.gguf.part"))?, partial);
            Ok(())
        })
        .await
    });
    timeout(Duration::from_secs(5), updates.changed()).await??;
    assert_eq!(
        updates.borrow().step,
        Some("Waiting for another setup to finish")
    );
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
    let (progress, _) = watch::channel(Progress::default());
    let _owner = Installation::lock(directory.path(), &progress).await?;
    let (progress, mut updates) = watch::channel(Progress::default());
    let (cancel, cancelled) = async_channel::bounded::<Infallible>(1);
    let root = directory.path().to_path_buf();
    let waiting = tokio::spawn(async move {
        Installation::run(&root, &progress, &cancelled, async |_| {
            anyhow::bail!("A waiting setup entered the owned directory")
        })
        .await
    });
    timeout(Duration::from_secs(5), updates.changed()).await??;
    drop(cancel);
    let result: Option<anyhow::Result<()>> = timeout(Duration::from_secs(5), waiting).await??;
    assert!(result.is_none());
    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("setup.lock"))?;
    assert!(matches!(
        contender.try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    Ok(())
}

#[tokio::test]
async fn setup_names_the_directory_it_cannot_prepare() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("file"), b"")?;
    let beneath_a_file = directory.path().join("file").join("speakeasy");
    let lock_is_a_directory = directory.path().join("speakeasy");
    fs::create_dir_all(lock_is_a_directory.join("setup.lock"))?;
    let (progress, _) = watch::channel(Progress::default());
    for root in [beneath_a_file, lock_is_a_directory] {
        let failure = Installation::lock(&root, &progress)
            .await
            .err()
            .context("Setup prepared a directory it cannot lock")?
            .to_string();
        assert!(
            failure.starts_with(&format!("Cannot prepare {}: ", root.display())),
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
    let (progress, _) = watch::channel(Progress::default());
    let (_cancel, cancelled) = async_channel::bounded::<Infallible>(1);
    let installed = Installation::run(root.path(), &progress, &cancelled, async |installation| {
        install(root.path(), &progress, installation).await
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
    let result = worker.transcribe(wav, "en").await;
    worker.stop().await;
    // Never include the recognized text in failure output.
    assert!(
        result?.to_lowercase().contains("ask not what your country"),
        "Fixture recognition did not contain the expected phrase"
    );
    Ok(())
}
