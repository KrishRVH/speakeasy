//! Automatic setup of NeMo-Speech.cpp's Metal build and the Parakeet v3 model.
//!
//! A packaged app carries Speakeasy's own engine build, which setup copies out of the bundle. Every
//! download, including NVIDIA's engine build for an unpackaged app, is pinned by URL, size, and
//! SHA-256, resumes after interruption, and is renamed into place only once verified.

use std::{
    convert::Infallible,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, bail};
use reqwest::{Client, StatusCode, header::RANGE};
use sha2::{Digest, Sha256};
use speakeasy_platform::OwnedThread;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::watch,
    time::timeout,
};

use crate::{
    child::{kill_and_reap, owned_command},
    config::{Config, Engine},
};

const MODEL: Download = Download {
    url: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/541d1f99c6b0c3cd0b11a95167540bb8edefd82b/parakeet-tdt-0.6b-v3.q8_0.gguf",
    size: 713_975_456,
    sha256: "e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e",
};

const METAL: Download = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-macos-aarch64-metal.tar.gz",
    size: 3_465_028,
    sha256: "f1dff4f9dd9c96214f8cb78b982812459132df8a4ad1a42409fd94de4a366244",
};

const EXECUTABLE: &str = "nemo-speech/bin/nemo-speech";

struct Download<'a> {
    url: &'a str,
    size: u64,
    sha256: &'a str,
}

/// What setup is doing. No step yet means it is checking this machine; `done` and `total` count the
/// bytes of the current step.
#[derive(Clone, Copy, Default)]
pub struct Progress {
    /// The current step's description.
    pub step: Option<&'static str>,
    /// Bytes completed in the current step.
    pub done: u64,
    /// Bytes the current step needs.
    pub total: u64,
}

/// The engine build setup kept, and whether it should run on the GPU.
struct ChosenEngine {
    executable: PathBuf,
    use_gpu: bool,
}

/// The engine and model a finished setup chose for this machine.
pub struct Installed {
    engine_executable: PathBuf,
    model: PathBuf,
    use_gpu: bool,
}

impl Installed {
    /// Selects the installed engine and model in `config`, like a manual choice.
    pub fn apply_to(self, config: &mut Config) {
        config.engine = Engine::Parakeet;
        config.engine_executable = self.engine_executable;
        config.model = self.model;
        config.use_gpu = self.use_gpu;
    }
}

/// A setup running on its own thread. Dropping it cancels the setup and joins the thread;
/// cancellation keeps partial downloads for the next run.
pub struct Setup {
    thread: OwnedThread,
    updates: watch::Receiver<Progress>,
    result: async_channel::Receiver<anyhow::Result<Installed>>,
    cancel: async_channel::Sender<Infallible>,
}

impl Setup {
    /// Starts automatic setup on its own thread.
    ///
    /// # Errors
    /// Returns the operating system's refusal to create the thread.
    pub fn start() -> anyhow::Result<Self> {
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
                    async |installation| {
                        install(&root, bundled_engine(), &progress, installation).await
                    },
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

    /// The latest progress.
    #[must_use]
    pub fn progress(&self) -> Progress {
        *self.updates.borrow()
    }

    /// A receiver that wakes whenever progress changes.
    #[must_use]
    pub fn progress_changes(&self) -> watch::Receiver<Progress> {
        self.updates.clone()
    }

    /// Resolves once the thread has stopped, with its result, or `None` when it stopped without
    /// one.
    pub fn outcome(&self) -> impl Future<Output = Option<anyhow::Result<Installed>>> + use<> {
        let result = self.result.clone();
        let stopped = self.stopped();
        async move {
            let outcome = result.recv().await.ok();
            stopped.await;
            outcome
        }
    }

    /// Resolves once the setup thread has exited.
    pub fn stopped(&self) -> impl Future<Output = ()> + use<> {
        self.thread.exited()
    }

    /// Whether the setup thread has exited.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Cancels setup without waiting; partial downloads remain for the next run.
    pub fn request_stop(&self) {
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
        fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("setup.lock"))?;
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
                Err(error) => return Err(error.into()),
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

/// Engines and models share the settings directory's parent but stay out of the settings file.
fn data_root() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(
            || PathBuf::from("."),
            |home| PathBuf::from(home).join("Library/Application Support"),
        )
        .join("speakeasy")
}

async fn install(
    root: &Path,
    bundled: Option<PathBuf>,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<Installed> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()?;
    let engine = choose_engine(&client, bundled, root, progress, installation).await?;
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

/// Installs the bundled engine build, or NVIDIA's Metal build without one, and uses the GPU when
/// the engine's own doctor confirms it works.
async fn choose_engine(
    client: &Client,
    bundled: Option<PathBuf>,
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<ChosenEngine> {
    let metal = match bundled {
        Some(build) => copy_out(&build, root, progress, installation).await?,
        None => unpack(client, &METAL, root, progress, installation).await?,
    };
    let report = doctor(&metal, installation)
        .await
        .context("The speech engine cannot run on this Mac.")?;
    Ok(ChosenEngine {
        executable: keep_only(root, &metal)?,
        use_gpu: is_accelerated(&report),
    })
}

/// The engine build a packaged app carries: `Contents/Resources/engine/<build>/nemo-speech`.
fn bundled_engine() -> Option<PathBuf> {
    let contents = std::env::current_exe()
        .ok()?
        .parent()?
        .parent()?
        .to_path_buf();
    fs::read_dir(contents.join("Resources/engine"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|build| build.join(EXECUTABLE).is_file())
}

/// Copies a bundled engine build into its own directory under `engines/`, keeping the build's
/// name so a newer app's build installs beside the old one instead of reusing it.
async fn copy_out(
    build: &Path,
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<PathBuf> {
    let name = build
        .file_name()
        .context("The bundled speech engine has no name")?;
    let engines = root.join("engines");
    let directory = engines.join(name);
    if directory.join(EXECUTABLE).is_file() {
        return Ok(directory);
    }
    progress.send_replace(Progress {
        step: Some("Installing the speech engine"),
        ..Progress::default()
    });
    let mut staging = directory.clone().into_os_string();
    staging.push(".part");
    let staging = PathBuf::from(staging);
    remove_engine_directory(&staging)?;
    fs::create_dir_all(&engines)?;
    // ditto keeps the build's library symlinks, permissions, and signatures.
    let mut command = owned_command("/usr/bin/ditto");
    command.arg(build).arg(&staging).stdout(Stdio::null());
    let (status, _) = installation.output(&mut command).await?;
    if !status.success() {
        bail!("Cannot install the speech engine. Check free disk space, then try again.");
    }
    remove_engine_directory(&directory)?;
    fs::rename(&staging, &directory)?;
    Ok(directory)
}

/// Removes every other engine build and returns this build's executable.
fn keep_only(root: &Path, directory: &Path) -> anyhow::Result<PathBuf> {
    for entry in fs::read_dir(root.join("engines"))?.flatten() {
        if entry.path() != directory {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Failure to prune an unused engine cache must not prevent the verified selected engine from running"
            )]
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    Ok(directory.join(EXECUTABLE))
}

async fn doctor(directory: &Path, installation: &mut Installation) -> Option<serde_json::Value> {
    let mut command = owned_command(directory.join(EXECUTABLE));
    command.args(["doctor", "--json"]).stdout(Stdio::piped());
    let output = timeout(Duration::from_secs(60), installation.output(&mut command)).await;
    installation.stop().await;
    let (status, stdout) = output.ok()?.ok()?;
    if !status.success() {
        return None;
    }
    serde_json::from_slice(&stdout).ok()
}

fn is_accelerated(report: &serde_json::Value) -> bool {
    report["accelerator_available"] == true && report["driver_runtime_compatible"] == true
}

/// Downloads and extracts an engine archive into its own directory.
async fn unpack(
    client: &Client,
    archive: &Download<'_>,
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<PathBuf> {
    let name = file_name(archive.url);
    let stem = name
        .strip_suffix(".zip")
        .or_else(|| name.strip_suffix(".tar.gz"))
        .unwrap_or(name);
    let engines = root.join("engines");
    let directory = engines.join(stem);
    if directory.join(EXECUTABLE).is_file() {
        return Ok(directory);
    }
    let file = fetch(
        client,
        archive,
        &root.join("downloads"),
        "Downloading the speech engine",
        progress,
    )
    .await?;
    progress.send_replace(Progress {
        step: Some("Unpacking the speech engine"),
        ..Progress::default()
    });
    let staging = engines.join(format!("{stem}.part"));
    remove_engine_directory(&staging)?;
    fs::create_dir_all(&staging)?;
    // The system tar reads the release archive; a tar earlier in PATH may not.
    let mut command = owned_command("/usr/bin/tar");
    command
        .arg("-xf")
        .arg(&file)
        .arg("-C")
        .arg(&staging)
        .stdout(Stdio::null());
    let (status, _) = installation.output(&mut command).await?;
    if !status.success() {
        bail!("Cannot unpack the speech engine. Check free disk space, then try again.");
    }
    remove_engine_directory(&directory)?;
    fs::rename(&staging, &directory)?;
    #[expect(
        clippy::let_underscore_must_use,
        reason = "The archive is only a cache after the verified engine is installed; pruning it is optional"
    )]
    let _ = fs::remove_file(&file);
    Ok(directory)
}

fn remove_engine_directory(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "Cannot replace {}. Close programs using the speech engine and check directory permissions, then try again.",
                path.display()
            )
        }),
    }
}

/// Downloads into `directory`, resuming a partial file when the server honors the range. Returns
/// the verified file.
async fn fetch(
    client: &Client,
    download: &Download<'_>,
    directory: &Path,
    step: &'static str,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<PathBuf> {
    let name = file_name(download.url);
    let path = directory.join(name);
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(directory)
        .with_context(|| format!("Cannot write to {}", directory.display()))?;
    let partial = directory.join(format!("{name}.part"));
    let mut done = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
    if done == download.size {
        let hasher = hash_partial(&partial, done, progress).await?;
        if hex_digest(&hasher.finalize()) == download.sha256 {
            fs::rename(&partial, &path)?;
            return Ok(path);
        }
        fs::remove_file(&partial)?;
        done = 0;
    } else if done > download.size {
        done = 0;
    }
    let mut request = client.get(download.url);
    if done > 0 {
        request = request.header(RANGE, format!("bytes={done}-"));
    }
    let mut response = request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .context("Cannot download. Check your internet connection, then try again.")?;
    let (mut hasher, mut file) = if done > 0 && response.status() == StatusCode::PARTIAL_CONTENT {
        (
            hash_partial(&partial, done, progress).await?,
            OpenOptions::new().append(true).open(&partial)?,
        )
    } else {
        done = 0;
        (Sha256::new(), File::create(&partial)?)
    };
    progress.send_replace(Progress {
        step: Some(step),
        done,
        total: download.size,
    });
    while let Some(chunk) = response
        .chunk()
        .await
        .context("The download stopped. Check your internet connection, then try again.")?
    {
        done = done
            .checked_add(chunk.len() as u64)
            .context("The download exceeded its published size. Try again.")?;
        if done > download.size {
            break;
        }
        hasher.update(&chunk);
        file.write_all(&chunk)?;
        progress.send_modify(|progress| progress.done = done);
    }
    file.sync_all()?;
    drop(file);
    if done != download.size || hex_digest(&hasher.finalize()) != download.sha256 {
        fs::remove_file(&partial).context(
            "Cannot remove an invalid download. Check directory permissions, then try again.",
        )?;
        bail!("A download did not match its published checksum. Try again.");
    }
    fs::rename(&partial, &path)?;
    Ok(path)
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0xF])
        .filter_map(|digit| char::from_digit(digit.into(), 16))
        .collect()
}

async fn hash_partial(
    partial: &Path,
    size: u64,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<Sha256> {
    progress.send_replace(Progress {
        step: Some("Verifying downloaded data"),
        done: 0,
        total: size,
    });
    let mut file = tokio::fs::File::open(partial).await?;
    // Every asynchronous read is a blocking-pool round trip, so read a MiB at a time.
    let mut buffer = vec![0; 1024 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let count = file.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let chunk = buffer
            .get(..count)
            .context("Cannot verify the downloaded file: the read size is invalid")?;
        hasher.update(chunk);
        progress.send_modify(|progress| {
            progress.done = progress.done.saturating_add(count as u64);
        });
        // Cached reads can complete without yielding; let cancellation run between chunks.
        tokio::task::yield_now().await;
    }
    Ok(hasher)
}

fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    use super::*;
    use crate::{local_speech::LocalSpeech, ports::Speech};

    const BODY: &[u8] = b"Say the word. The door opens, and what is said stays inside.";

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

    #[tokio::test]
    async fn unpack_reports_an_unreplaceable_staging_directory() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::create_dir_all(root.join("downloads"))?;
        fs::create_dir_all(root.join("engines"))?;
        fs::write(root.join("downloads/fixture.tar.gz"), b"unused archive")?;
        fs::write(root.join("engines/fixture.part"), b"not a directory")?;
        let (progress, _) = watch::channel(Progress::default());
        let mut installation = Installation {
            child: None,
            _lock: Installation::lock(root, &progress).await?,
        };
        let archive = Download {
            url: "http://127.0.0.1:0/fixture.tar.gz",
            size: 0,
            sha256: "unused: replacement must fail before extraction",
        };
        let failure = unpack(&Client::new(), &archive, root, &progress, &mut installation)
            .await
            .unwrap_err();
        let message = failure.to_string();
        assert!(message.contains("Cannot replace"));
        assert!(message.contains("check directory permissions"));
        assert_eq!(
            fs::read(root.join("engines/fixture.part"))?,
            b"not a directory"
        );
        assert!(installation.child.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn a_bundled_engine_is_copied_out_once_with_its_library_links() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let build = directory
            .path()
            .join("bundle/nemo-speech-0.1.0-speakeasy-fixture");
        fs::create_dir_all(build.join("nemo-speech/bin"))?;
        fs::create_dir_all(build.join("nemo-speech/lib"))?;
        fs::write(build.join(EXECUTABLE), b"#!/bin/sh\n")?;
        fs::write(build.join("nemo-speech/lib/libengine.1.dylib"), b"library")?;
        std::os::unix::fs::symlink(
            "libengine.1.dylib",
            build.join("nemo-speech/lib/libengine.dylib"),
        )?;
        let root = directory.path().join("support");
        let (progress, _) = watch::channel(Progress::default());
        let mut installation = Installation {
            child: None,
            _lock: Installation::lock(&root, &progress).await?,
        };
        let installed = copy_out(&build, &root, &progress, &mut installation).await?;
        assert_eq!(
            installed,
            root.join("engines/nemo-speech-0.1.0-speakeasy-fixture")
        );
        assert!(installed.join(EXECUTABLE).is_file());
        let link = installed.join("nemo-speech/lib/libengine.dylib");
        assert_eq!(fs::read_link(&link)?, Path::new("libengine.1.dylib"));
        assert!(
            !root
                .join("engines/nemo-speech-0.1.0-speakeasy-fixture.part")
                .exists()
        );

        // An installed build is reused as is, never recopied over a running engine.
        fs::write(build.join(EXECUTABLE), b"changed")?;
        assert_eq!(
            copy_out(&build, &root, &progress, &mut installation).await?,
            installed
        );
        assert_eq!(fs::read(installed.join(EXECUTABLE))?, b"#!/bin/sh\n");
        assert!(installation.child.is_none());
        Ok(())
    }

    #[test]
    #[ignore = "Owned subprocess fixture for setup cancellation tests"]
    fn setup_child_fixture() -> anyhow::Result<()> {
        let root = PathBuf::from(
            std::env::var_os("SPEAKEASY_SETUP_TEST_ROOT").context("Missing fixture root")?,
        );
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
        fs::write(root.join("model.gguf.part"), &BODY[..20])?;
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
                assert_eq!(fs::read(second_root.join("model.gguf.part"))?, &BODY[..20]);
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

    /// Serves `BODY` on loopback, honoring `Range: bytes=N-`. The receiver yields the range start
    /// each request asked for.
    async fn serve_body() -> anyhow::Result<(String, async_channel::Receiver<Option<u64>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let (requests, starts) = async_channel::unbounded();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if respond(stream, &requests).await.is_err() {
                    break;
                }
            }
        });
        Ok((url, starts))
    }

    async fn respond(
        mut stream: TcpStream,
        requests: &async_channel::Sender<Option<u64>>,
    ) -> anyhow::Result<()> {
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.ends_with(b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await?;
            anyhow::ensure!(read > 0, "request ended early");
            request.extend_from_slice(&buffer[..read]);
        }
        let start = String::from_utf8(request)?
            .to_lowercase()
            .lines()
            .find_map(|line| line.strip_prefix("range: bytes="))
            .and_then(|range| range.trim_end_matches('-').parse::<usize>().ok());
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The fixture still serves its response when a test deliberately discards the optional request observer"
        )]
        let _ = requests.send(start.map(|start| start as u64)).await;
        let head = match start {
            Some(start) => format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{}\r\n",
                BODY.len().saturating_sub(1),
                BODY.len()
            ),
            None => "HTTP/1.1 200 OK\r\n".into(),
        };
        let body = &BODY[start.unwrap_or(0)..];
        let head = format!(
            "{head}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body).await?;
        Ok(())
    }

    fn sha256(bytes: &[u8]) -> String {
        hex_digest(&Sha256::digest(bytes))
    }

    #[tokio::test]
    async fn fetch_promotes_a_verified_complete_partial_without_a_request() -> anyhow::Result<()> {
        let (base, requests) = serve_body().await?;
        let url = format!("{base}/model.gguf");
        let sha = sha256(BODY);
        let download = Download {
            url: &url,
            size: BODY.len() as u64,
            sha256: &sha,
        };
        let directory = tempfile::tempdir()?;
        let partial = directory.path().join("model.gguf.part");
        fs::write(&partial, BODY)?;
        let (progress, _) = watch::channel(Progress::default());
        let path = fetch(
            &Client::new(),
            &download,
            directory.path(),
            "fixture",
            &progress,
        )
        .await?;
        assert_eq!(fs::read(path)?, BODY);
        assert!(!partial.exists());
        assert!(
            requests.try_recv().is_err(),
            "Complete verified audio model was downloaded again"
        );
        Ok(())
    }

    #[tokio::test]
    async fn fetch_replaces_a_corrupt_complete_partial() -> anyhow::Result<()> {
        let (base, requests) = serve_body().await?;
        let url = format!("{base}/model.gguf");
        let sha = sha256(BODY);
        let download = Download {
            url: &url,
            size: BODY.len() as u64,
            sha256: &sha,
        };
        let directory = tempfile::tempdir()?;
        fs::write(
            directory.path().join("model.gguf.part"),
            vec![0; BODY.len()],
        )?;
        let (progress, _) = watch::channel(Progress::default());
        let path = fetch(
            &Client::new(),
            &download,
            directory.path(),
            "fixture",
            &progress,
        )
        .await?;
        assert_eq!(fs::read(path)?, BODY);
        assert_eq!(requests.recv().await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_during_verification_keeps_the_partial_for_retry() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let partial = directory.path().join("model.gguf.part");
        let bytes = vec![0; 4 * 1024 * 1024];
        fs::write(&partial, &bytes)?;
        let sha = sha256(&bytes);
        let download = Download {
            url: "http://127.0.0.1:1/model.gguf",
            size: bytes.len() as u64,
            sha256: &sha,
        };
        let (progress, mut updates) = watch::channel(Progress::default());
        let (cancel, cancelled) = async_channel::bounded::<Infallible>(1);
        let cancel_when_verifying = async {
            updates
                .wait_for(|progress| {
                    progress.step == Some("Verifying downloaded data")
                        && progress.done > 0
                        && progress.done < progress.total
                })
                .await?;
            drop(cancel);
            Ok::<_, anyhow::Error>(())
        };
        let work = Installation::run(directory.path(), &progress, &cancelled, async |_| {
            fetch(
                &Client::new(),
                &download,
                directory.path(),
                "fixture",
                &progress,
            )
            .await
        });
        let (result, cancellation) = timeout(Duration::from_secs(5), async {
            tokio::join!(work, cancel_when_verifying)
        })
        .await?;
        cancellation?;
        assert!(result.is_none());
        assert_eq!(fs::read(partial)?, bytes);
        assert!(!directory.path().join("model.gguf").exists());
        Ok(())
    }

    #[tokio::test]
    async fn fetch_resumes_a_partial_download_and_verifies_the_whole_file() -> anyhow::Result<()> {
        let (base, requests) = serve_body().await?;
        let url = format!("{base}/model.gguf");
        let sha = sha256(BODY);
        let download = Download {
            url: &url,
            size: BODY.len() as u64,
            sha256: &sha,
        };
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("model.gguf.part"), &BODY[..20])?;
        let (progress, _) = watch::channel(Progress::default());
        let path = fetch(
            &Client::new(),
            &download,
            directory.path(),
            "fixture",
            &progress,
        )
        .await?;
        assert_eq!(requests.recv().await?, Some(20));
        assert_eq!(fs::read(&path)?, BODY);
        assert!(!directory.path().join("model.gguf.part").exists());
        assert_eq!(progress.borrow().done, BODY.len() as u64);
        Ok(())
    }

    #[tokio::test]
    async fn fetch_discards_a_download_that_fails_its_checksum() -> anyhow::Result<()> {
        let (base, _) = serve_body().await?;
        let url = format!("{base}/model.gguf");
        let sha = sha256(b"a different model");
        let download = Download {
            url: &url,
            size: BODY.len() as u64,
            sha256: &sha,
        };
        let directory = tempfile::tempdir()?;
        let (progress, _) = watch::channel(Progress::default());
        let result = fetch(
            &Client::new(),
            &download,
            directory.path(),
            "fixture",
            &progress,
        )
        .await;
        let error = result.err().map(|error| format!("{error:#}"));
        assert!(
            error
                .as_deref()
                .is_some_and(|error| error.contains("checksum")),
            "{error:?}"
        );
        assert!(!directory.path().join("model.gguf").exists());
        assert!(!directory.path().join("model.gguf.part").exists());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Downloads about 0.8 GB; requires SPEAKEASY_FIXTURE_WAV (whisper.cpp samples/jfk.wav) and optionally SPEAKEASY_FIXTURE_ENGINE (a scripts/build-engine.sh build)"]
    async fn install_chooses_an_engine_that_recognizes_fixture_speech() -> anyhow::Result<()> {
        let wav = fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
        let bundled = std::env::var_os("SPEAKEASY_FIXTURE_ENGINE").map(PathBuf::from);
        let root = tempfile::tempdir()?;
        let (progress, _) = watch::channel(Progress::default());
        let (_cancel, cancelled) = async_channel::bounded::<Infallible>(1);
        let installed =
            Installation::run(root.path(), &progress, &cancelled, async |installation| {
                install(root.path(), bundled, &progress, installation).await
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
