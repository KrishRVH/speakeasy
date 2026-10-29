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
    io,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};

use anyhow::anyhow;
use speakeasy_platform::StoppableThread;
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
    http::{Trust, client_builder},
};

const MODEL: Download<'static> = Download {
    url: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/541d1f99c6b0c3cd0b11a95167540bb8edefd82b/parakeet-tdt-0.6b-v3.q8_0.gguf",
    size: 713_975_456,
    sha256: "e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e",
};

/// What setup is doing; `done` and `total` count the bytes of the current step when it has a size.
#[derive(Clone, Copy)]
pub(crate) struct Progress {
    pub step: &'static str,
    pub done: u64,
    pub total: u64,
}

impl Progress {
    const fn new(step: &'static str) -> Self {
        Self {
            step,
            done: 0,
            total: 0,
        }
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new("Checking this machine")
    }
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
    thread: StoppableThread,
    updates: watch::Receiver<Progress>,
    result: async_channel::Receiver<anyhow::Result<Installed>>,
}

impl Setup {
    pub(crate) fn start() -> anyhow::Result<Self> {
        Self::start_in(crate::paths::data_root()?, install)
    }

    fn start_in(
        root: PathBuf,
        work: impl AsyncFnOnce(
            &Path,
            &watch::Sender<Progress>,
            &mut Installation,
        ) -> anyhow::Result<Installed>
        + Send
        + 'static,
    ) -> anyhow::Result<Self> {
        Self::spawn(move |progress, cancelled| {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => return Some(Err(error.into())),
            };
            runtime.block_on(Installation::run(
                &root,
                &progress,
                &cancelled,
                async |installation| work(&root, &progress, installation).await,
            ))
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
        let thread = StoppableThread::spawn("setup", move |cancelled| {
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
        self.thread.stopped()
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    pub(crate) fn request_stop(&self) {
        self.thread.request_stop();
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
        let installation = tokio::select! {
            biased;
            _ = cancelled.recv() => return None,
            installation = Self::acquire(root, progress) => installation,
        };
        let mut installation = match installation {
            Ok(installation) => installation,
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

    /// Creates `root` if needed and waits for its setup lock, reporting the wait.
    async fn acquire(root: &Path, progress: &watch::Sender<Progress>) -> anyhow::Result<Self> {
        fs::create_dir_all(root).map_err(file_error(root))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("setup.lock"))
            .map_err(file_error(root))?;
        loop {
            match lock.try_lock() {
                Ok(()) => {
                    progress.send_replace(Progress::default());
                    return Ok(Self {
                        child: None,
                        _lock: lock,
                    });
                },
                Err(fs::TryLockError::WouldBlock) => {
                    progress.send_replace(Progress::new("Waiting for another setup to finish"));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                },
                Err(fs::TryLockError::Error(error)) => return Err(file_error(root)(error)),
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
            Ok::<_, io::Error>(bytes)
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

async fn install(
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<Installed> {
    let client = client_builder(Trust::System)?
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
    let use_gpu = engine.use_gpu;
    Ok(Installed {
        engine_executable: engine.keep(root)?,
        model,
        use_gpu,
    })
}

/// Explains a failed file operation in `directory`, with the system's reason and the remedy it
/// calls for, in one message that reads whole without its source chain.
fn file_error(directory: &Path) -> impl FnOnce(io::Error) -> anyhow::Error {
    move |error| {
        anyhow!(
            "Setup cannot use {}: {error}. {}",
            directory.display(),
            crate::paths::remedy(&error)
        )
    }
}

fn system32() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join("System32")
}

#[cfg(test)]
mod tests;
