//! Automatic setup installs the NeMo-Speech.cpp build that suits this machine and the Parakeet v3
//! model. `Setup` runs it on an owned thread that cancels and joins on drop, and `Installation`
//! holds the lock on its directory until its native child is reaped, so one setup installs there at
//! a time.

mod archive;
mod download;
mod engine;

use std::{
    convert::Infallible,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};

use anyhow::anyhow;
use reqwest::Client;
use speakeasy_platform::OwnedThread;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::watch,
};

use self::{
    download::{Download, fetch},
    engine::choose_engine,
};
use crate::{
    child::kill_and_reap,
    config::{Config, Engine},
};

const MODEL: Download = Download {
    url: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/541d1f99c6b0c3cd0b11a95167540bb8edefd82b/parakeet-tdt-0.6b-v3.q8_0.gguf",
    size: 713_975_456,
    sha256: "e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e",
};

/// What setup is doing. No step yet means it is checking this machine; `done` and `total` count the
/// bytes of the current step.
#[derive(Clone, Copy, Default)]
pub(crate) struct Progress {
    pub step: Option<&'static str>,
    pub done: u64,
    pub total: u64,
}

/// The engine and model a finished setup chose for this machine.
pub(crate) struct Installed {
    engine_executable: PathBuf,
    model: PathBuf,
    use_gpu: bool,
}

impl Installed {
    pub(crate) fn apply_to(self, config: &mut Config) {
        config.engine = Engine::Parakeet;
        config.engine_executable = self.engine_executable;
        config.model = self.model;
        config.use_gpu = self.use_gpu;
    }
}

/// A setup running on its own thread. Dropping it cancels the setup and joins the thread;
/// cancellation keeps partial downloads for the next run.
pub(crate) struct Setup {
    thread: OwnedThread,
    updates: watch::Receiver<Progress>,
    result: async_channel::Receiver<anyhow::Result<Installed>>,
    cancel: async_channel::Sender<Infallible>,
}

impl Setup {
    pub(crate) fn start() -> anyhow::Result<Self> {
        Self::spawn(|progress, cancelled| {
            let root = data_root();
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(Installation::run(
                    &root,
                    &progress,
                    &cancelled,
                    async |installation| install(&root, &progress, installation).await,
                )),
                Err(error) => Some(Err(error.into())),
            }
        })
    }

    fn spawn(
        work: impl FnOnce(
            watch::Sender<Progress>,
            async_channel::Receiver<Infallible>,
        ) -> Option<anyhow::Result<Installed>>
        + Send
        + 'static,
    ) -> anyhow::Result<Self> {
        let (progress, updates) = watch::channel(Progress::default());
        let (finish, result) = async_channel::bounded(1);
        let (cancel, cancelled) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("setup", move || {
            if let Some(result) = work(progress, cancelled) {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A released owner no longer wants the result; native cleanup has already finished"
                )]
                let _ = finish.try_send(result);
            }
        })?;
        Ok(Self {
            thread,
            updates,
            result,
            cancel,
        })
    }

    pub(crate) fn progress(&self) -> Progress {
        *self.updates.borrow()
    }

    pub(crate) fn progress_changes(&self) -> watch::Receiver<Progress> {
        self.updates.clone()
    }

    /// Resolves once the thread has stopped, with its result, or `None` when it stopped without
    /// one.
    pub(crate) fn outcome(
        &self,
    ) -> impl Future<Output = Option<anyhow::Result<Installed>>> + use<> {
        let result = self.result.clone();
        let stopped = self.stopped();
        async move {
            let outcome = result.recv().await.ok();
            stopped.await;
            outcome
        }
    }

    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + use<> {
        self.thread.exited()
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    pub(crate) fn request_stop(&self) {
        self.cancel.close();
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Keeps the shared setup directory locked until its native child is reaped.
struct Installation {
    // Declared first, so an unwinding drop kills the child before releasing the lock.
    child: Option<Child>,
    _lock: File,
}

impl Installation {
    async fn run<T>(
        root: &Path,
        progress: &watch::Sender<Progress>,
        cancelled: &async_channel::Receiver<Infallible>,
        work: impl AsyncFnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> Option<anyhow::Result<T>> {
        let lock = tokio::select! {
            biased;
            _ = cancelled.recv() => return None,
            lock = Self::lock(root, progress) => lock,
        };
        let mut installation = match lock {
            Ok(lock) => Self {
                child: None,
                _lock: lock,
            },
            Err(error) => return Some(Err(error)),
        };
        let result = tokio::select! {
            biased;
            _ = cancelled.recv() => None,
            result = work(&mut installation) => Some(result),
        };
        installation.stop().await;
        result
    }

    async fn lock(root: &Path, progress: &watch::Sender<Progress>) -> anyhow::Result<File> {
        let cannot_prepare =
            |error: std::io::Error| anyhow!("Cannot prepare {}: {error}", root.display());
        fs::create_dir_all(root).map_err(cannot_prepare)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("setup.lock"))
            .map_err(cannot_prepare)?;
        loop {
            match lock.try_lock() {
                Ok(()) => {
                    progress.send_replace(Progress::default());
                    return Ok(lock);
                },
                Err(fs::TryLockError::WouldBlock) => {
                    progress.send_replace(Progress {
                        step: Some("Waiting for another setup to finish"),
                        ..Progress::default()
                    });
                    tokio::time::sleep(Duration::from_millis(100)).await;
                },
                Err(fs::TryLockError::Error(error)) => return Err(cannot_prepare(error)),
            }
        }
    }

    /// Runs `command` as the owned child; returns its exit status and whatever it piped to stdout.
    async fn output(&mut self, command: &mut Command) -> anyhow::Result<(ExitStatus, Vec<u8>)> {
        let child = self.child.insert(command.spawn()?);
        let mut pipe = child.stdout.take();
        let output = tokio::try_join!(child.wait(), async {
            let mut bytes = Vec::new();
            if let Some(pipe) = &mut pipe {
                pipe.read_to_end(&mut bytes).await?;
            }
            Ok::<_, std::io::Error>(bytes)
        })?;
        self.child = None;
        Ok(output)
    }

    async fn stop(&mut self) {
        // Reap in place: a caller cancelled mid-wait leaves the child here for `run` to reap.
        if let Some(child) = &mut self.child {
            kill_and_reap(child).await;
        }
        self.child = None;
    }
}

/// Engines, models, and Shows are machine-local, so they stay out of roaming settings.
pub(crate) fn data_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(target_os = "linux")]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    base.unwrap_or_else(|| PathBuf::from(".")).join("speakeasy")
}

async fn install(
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<Installed> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()?;
    let engine = choose_engine(&client, root, progress, installation).await?;
    let model = fetch(
        &client,
        &MODEL,
        &root.join("models"),
        "Downloading the Parakeet model",
        progress,
    )
    .await?;
    Ok(Installed {
        engine_executable: engine.executable,
        model,
        use_gpu: engine.use_gpu,
    })
}

fn system32() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join("System32")
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use anyhow::Context as _;
    use tokio::time::timeout;

    use super::*;
    use crate::{local_speech::LocalSpeech, ports::Speech};

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
        let mut retiring = vec![setup];
        cleaning.recv_timeout(Duration::from_secs(2))?;
        assert!(
            timeout(Duration::from_millis(30), retiring[0].stopped())
                .await
                .is_err(),
            "A cancelled setup reported stopping before its cleanup finished"
        );
        assert!(!retiring[0].is_finished());
        release.send(())?;
        timeout(Duration::from_secs(2), stopped).await?;
        retiring.clear();
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
    async fn cancellation_stops_the_child_before_another_setup_owns_the_directory()
    -> anyhow::Result<()> {
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
        let installed =
            Installation::run(root.path(), &progress, &cancelled, async |installation| {
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
}
