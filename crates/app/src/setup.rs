//! Automatic setup installs the NeMo-Speech.cpp build that suits this machine
//! and the Parakeet v3 model. Every download is pinned by URL, size, and
//! SHA-256, resumes after interruption, and is renamed into place only once
//! verified.

use crate::config::{Config, Engine};
use anyhow::{Context, bail};
use reqwest::{Client, StatusCode, header::RANGE};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    thread,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::watch,
    time::timeout,
};

#[derive(Clone, Default)]
pub(crate) struct Progress {
    pub step: &'static str,
    pub done: u64,
    pub total: u64,
}

struct Download<'a> {
    url: &'a str,
    size: u64,
    sha256: &'a str,
}

const MODEL: Download = Download {
    url: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/541d1f99c6b0c3cd0b11a95167540bb8edefd82b/parakeet-tdt-0.6b-v3.q8_0.gguf",
    size: 713_975_456,
    sha256: "e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e",
};
#[cfg(target_os = "windows")]
const CUDA: Download = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-cuda.zip",
    size: 106_044_768,
    sha256: "ba024204e76ca2fa4eefa8787506c3c49e418147f627f60cf9206a582b60089c",
};
#[cfg(target_os = "windows")]
const VULKAN: Download = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-vulkan.zip",
    size: 21_967_184,
    sha256: "b5e7b04a637da4eb25a60253e2db65774998e8dfb48c08b4db763009b82ac7ac",
};
#[cfg(target_os = "windows")]
const CPU: Download = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-cpu.zip",
    size: 4_730_421,
    sha256: "5e4ea81046012edcd77fd8848de8eefb5a4ba38cc26f52eb544ab184695a75d6",
};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CUDA: Download = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-x86_64-cuda.tar.gz",
    size: 107_310_946,
    sha256: "e68628f396489c98fb353e070efaea5bc4977409ae7734fce56c251a79e29147",
};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const VULKAN: Download = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-x86_64-vulkan.tar.gz",
    size: 18_014_113,
    sha256: "ce7b7c3c8771cb7450b26e6d4bd8fb2c5e35bcd9fe0076387f35052e9b9523ae",
};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CPU: Download = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-x86_64-cpu.tar.gz",
    size: 4_583_913,
    sha256: "0f74131d631ad2c694cf0ec53490866bb6461147959589a69fb6fc231944065b",
};
#[cfg(target_os = "macos")]
const METAL: Download = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-macos-aarch64-metal.tar.gz",
    size: 3_465_028,
    sha256: "f1dff4f9dd9c96214f8cb78b982812459132df8a4ad1a42409fd94de4a366244",
};

#[cfg(target_os = "windows")]
const EXECUTABLE: &str = r"bin\nemo-speech.exe";
#[cfg(target_os = "macos")]
const EXECUTABLE: &str = "nemo-speech/bin/nemo-speech";
#[cfg(target_os = "linux")]
const EXECUTABLE: &str = "bin/nemo-speech";

// After a five-minute recording, Parakeet holds about 3.8 GB of GPU memory.
#[cfg(any(target_os = "windows", target_os = "linux", test))]
const GPU_MEMORY: u64 = 6_000_000_000;

/// A running setup. Cancellation preserves partial downloads for the next run.
/// The owner waits for stopped before ordinary disposal; Quit joins cleanup.
pub(crate) struct Setup {
    pub progress: watch::Receiver<Progress>,
    pub result: async_channel::Receiver<anyhow::Result<Config>>,
    cancel: async_channel::Sender<()>,
    finished: async_channel::Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Setup {
    pub(crate) fn start() -> anyhow::Result<Self> {
        Self::spawn(|report, cancelled| {
            let root = root();
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(Installation::run(
                    &root,
                    &report,
                    &cancelled,
                    async |installation| install(&root, &report, installation).await,
                )),
                Err(error) => Some(Err(error.into())),
            }
        })
    }

    // A setup thread owns installation and native cleanup until acknowledgement.
    fn spawn(
        work: impl FnOnce(
            watch::Sender<Progress>,
            async_channel::Receiver<()>,
        ) -> Option<anyhow::Result<Config>>
        + Send
        + 'static,
    ) -> anyhow::Result<Self> {
        let (report, progress) = watch::channel(Progress::default());
        let (finish, result) = async_channel::bounded(1);
        let (cancel, cancelled) = async_channel::bounded::<()>(1);
        let (completed, finished) = async_channel::bounded(1);
        let thread = thread::Builder::new().name("setup".into()).spawn(move || {
            if let Some(result) = work(report, cancelled) {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "The single setup result has no caller after its receiver closes; cleanup must still acknowledge"
                )]
                let _ = finish.try_send(result);
            }
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A dropped setup owner needs no acknowledgement; the thread has completed native cleanup"
            )]
            let _ = completed.try_send(());
        })?;
        Ok(Self {
            progress,
            result,
            cancel,
            finished,
            thread: Some(thread),
        })
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }

    pub(crate) fn request_stop(&self) {
        self.cancel.close();
    }

    pub(crate) fn stopped(&self) -> impl std::future::Future<Output = ()> + use<> {
        let finished = self.finished.clone();
        async move {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Acknowledgement or sender disposal both mean the owned setup thread has stopped"
            )]
            let _ = finished.recv().await;
        }
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Drop cannot report a worker panic; joining still retains the owner until cleanup ends"
            )]
            let _ = thread.join();
        }
    }
}

/// Keeps the shared setup directory locked until its native child is reaped.
struct Installation {
    _lock: File,
    child: Option<Child>,
}

impl Installation {
    async fn run<T>(
        root: &Path,
        progress: &watch::Sender<Progress>,
        cancelled: &async_channel::Receiver<()>,
        work: impl AsyncFnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> Option<anyhow::Result<T>> {
        let lock = tokio::select! {
            biased;
            _ = cancelled.recv() => return None,
            lock = Self::lock(root, progress) => match lock {
                Ok(lock) => lock,
                Err(error) => return Some(Err(error)),
            },
        };
        let mut installation = Self {
            _lock: lock,
            child: None,
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
                Err(std::fs::TryLockError::WouldBlock) => {
                    progress.send_replace(Progress {
                        step: "Waiting for another setup to finish",
                        ..Progress::default()
                    });
                    tokio::time::sleep(Duration::from_millis(100)).await;
                },
                Err(error) => return Err(error.into()),
            }
        }
    }

    async fn output(&mut self, command: &mut Command) -> anyhow::Result<Output> {
        self.child = Some(command.spawn()?);
        let child = self.child.as_mut().context("Setup process did not start")?;
        let mut stdout = child.stdout.take();
        let (status, stdout) = tokio::try_join!(child.wait(), async {
            let mut bytes = Vec::new();
            if let Some(stdout) = &mut stdout {
                stdout.read_to_end(&mut bytes).await?;
            }
            Ok::<_, std::io::Error>(bytes)
        })?;
        self.child = None;
        Ok(Output {
            status,
            stdout,
            stderr: Vec::new(),
        })
    }

    async fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "An already exited process may reject termination; cleanup still waits and drops its kill-on-drop owner"
            )]
            let _ = child.start_kill();
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Cancellation has no result caller; disposal retains Tokio's kill-on-drop fallback after waiting"
            )]
            let _ = child.wait().await;
        }
        self.child = None;
    }
}

/// Engines and models are machine-local, so they stay out of roaming settings.
fn root() -> PathBuf {
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
) -> anyhow::Result<Config> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()?;
    let (engine_executable, use_gpu) = engine(&client, root, progress, installation).await?;
    let model = fetch(
        &client,
        &MODEL,
        &root.join("models"),
        "Downloading the Parakeet model",
        progress,
    )
    .await?;
    Ok(Config {
        engine: Engine::Parakeet,
        engine_executable,
        model,
        use_gpu,
        ..Config::default()
    })
}

/// Try CUDA with an NVIDIA driver, then Vulkan, then CPU. Doctor checks runtime
/// dependencies and accelerator availability; `LocalSpeech` verifies inference
/// before model readiness is reported.
#[cfg(any(
    target_os = "windows",
    all(target_os = "linux", target_arch = "x86_64")
))]
async fn engine(
    client: &Client,
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<(PathBuf, bool)> {
    if nvidia_driver() {
        let cuda = unpack(client, &CUDA, root, progress, installation).await?;
        if doctor(&cuda, installation)
            .await
            .is_some_and(|report| discrete_gpu(&report))
        {
            return keep(root, cuda, true);
        }
    }
    let vulkan = unpack(client, &VULKAN, root, progress, installation).await?;
    if let Some(report) = doctor(&vulkan, installation).await {
        return keep(root, vulkan, discrete_gpu(&report));
    }
    let cpu = unpack(client, &CPU, root, progress, installation).await?;
    #[cfg(target_os = "linux")]
    if doctor(&cpu, installation).await.is_none() {
        bail!(
            "The downloaded engine cannot run on this Linux CPU. Choose a portable NeMo-Speech.cpp build in Settings."
        );
    }
    keep(root, cpu, false)
}

#[cfg(target_os = "macos")]
async fn engine(
    client: &Client,
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<(PathBuf, bool)> {
    let metal = unpack(client, &METAL, root, progress, installation).await?;
    let report = doctor(&metal, installation)
        .await
        .context("The speech engine cannot run on this Mac.")?;
    keep(root, metal, accelerated(&report))
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    all(target_os = "linux", target_arch = "x86_64")
)))]
async fn engine(
    _: &Client,
    _: &Path,
    _: &watch::Sender<Progress>,
    _: &mut Installation,
) -> anyhow::Result<(PathBuf, bool)> {
    bail!(
        "Automatic Linux setup requires x86_64. Choose a local speech executable and model for this architecture."
    )
}

#[cfg(any(
    target_os = "windows",
    all(target_os = "linux", target_arch = "x86_64")
))]
fn nvidia_driver() -> bool {
    #[cfg(target_os = "windows")]
    {
        system32().join("nvidia-smi.exe").is_file()
    }
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc/driver/nvidia/version").is_file()
    }
}

#[cfg(target_os = "windows")]
fn system32() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or("C:\\Windows".into())).join("System32")
}

/// Removes other engine builds and returns the chosen executable.
fn keep(root: &Path, directory: PathBuf, gpu: bool) -> anyhow::Result<(PathBuf, bool)> {
    for entry in fs::read_dir(root.join("engines"))?.flatten() {
        if entry.path() != directory {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Failure to prune an unused engine cache must not prevent the verified selected engine from running"
            )]
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    Ok((directory.join(EXECUTABLE), gpu))
}

async fn doctor(directory: &Path, installation: &mut Installation) -> Option<serde_json::Value> {
    let mut command = Command::new(directory.join(EXECUTABLE));
    command
        .args(["doctor", "--json"])
        .stdout(Stdio::piped())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let output = timeout(Duration::from_secs(60), installation.output(&mut command)).await;
    installation.stop().await;
    let output = output.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn accelerated(report: &serde_json::Value) -> bool {
    report["accelerator_available"] == true && report["driver_runtime_compatible"] == true
}

#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn discrete_gpu(report: &serde_json::Value) -> bool {
    accelerated(report)
        && report["devices"].as_array().is_some_and(|devices| {
            devices.iter().any(|device| {
                device["type"] == "gpu"
                    && device["memory_total"].as_u64().unwrap_or(0) >= GPU_MEMORY
            })
        })
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
        step: "Unpacking the speech engine",
        ..Progress::default()
    });
    let staging = engines.join(format!("{stem}.part"));
    remove_engine_directory(&staging)?;
    fs::create_dir_all(&staging)?;
    // The system bsdtar reads both archive formats; a tar earlier in PATH may not.
    #[cfg(target_os = "windows")]
    let tar = system32().join("tar.exe");
    #[cfg(not(target_os = "windows"))]
    let tar = PathBuf::from("/usr/bin/tar");
    let mut command = Command::new(tar);
    command
        .arg("-xf")
        .arg(&file)
        .arg("-C")
        .arg(&staging)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    if !installation.output(&mut command).await?.status.success() {
        bail!("Cannot unpack the speech engine. Check free disk space, then try again.");
    }
    #[cfg(target_os = "linux")]
    normalize_linux_archive(&staging)?;
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

#[cfg(target_os = "linux")]
fn normalize_linux_archive(staging: &Path) -> anyhow::Result<()> {
    if staging.join(EXECUTABLE).is_file() {
        return Ok(());
    }
    let directories: Vec<_> = fs::read_dir(staging)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect();
    let [directory] = directories.as_slice() else {
        bail!("The engine archive has an unexpected layout.");
    };
    let directory = directory.path();
    if !directory.join(EXECUTABLE).is_file() {
        bail!("The engine archive has an unexpected layout.");
    }
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        fs::rename(entry.path(), staging.join(entry.file_name()))?;
    }
    fs::remove_dir(directory)?;
    Ok(())
}

/// Downloads into `directory`, resuming a partial file when the server honors
/// the range. Returns the verified file.
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
    let mut hasher = Sha256::new();
    let mut file = if done > 0 && response.status() == StatusCode::PARTIAL_CONTENT {
        hasher = hash_partial(&partial, done, progress).await?;
        OpenOptions::new().append(true).open(&partial)?
    } else {
        done = 0;
        File::create(&partial)?
    };
    progress.send_replace(Progress {
        step,
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
    let digest = hex_digest(&hasher.finalize());
    if done != download.size || digest != download.sha256 {
        fs::remove_file(&partial).context(
            "Cannot remove an invalid download. Check directory permissions, then try again.",
        )?;
        bail!("A download did not match its published checksum. Try again.");
    }
    fs::rename(&partial, &path)?;
    Ok(path)
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for digit in bytes.iter().flat_map(|byte| [byte >> 4, byte & 15]) {
        let ascii = if digit < 10 {
            b'0'.saturating_add(digit)
        } else {
            b'a'.saturating_add(digit.saturating_sub(10))
        };
        text.push(char::from(ascii));
    }
    text
}

async fn hash_partial(
    partial: &Path,
    size: u64,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<Sha256> {
    progress.send_replace(Progress {
        step: "Verifying downloaded data",
        done: 0,
        total: size,
    });
    let mut file = tokio::fs::File::open(partial).await?;
    // Each asynchronous file read crosses the blocking I/O executor. A bounded
    // MiB read avoids thousands of round trips when verifying a cached model.
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
        // Cached reads can complete immediately. Let setup cancellation run
        // between bounded hash chunks as well as while waiting for disk I/O.
        tokio::task::yield_now().await;
    }
    Ok(hasher)
}

fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    const BODY: &[u8] = b"Say the word. The door opens, and what is said stays inside.";

    #[tokio::test]
    async fn cancelled_setup_remains_owned_while_cleanup_is_pending() -> anyhow::Result<()> {
        let (entered, cleaning) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let setup = Setup::spawn(move |_, cancelled| {
            assert!(cancelled.recv_blocking().is_err());
            entered.send(()).unwrap();
            // Assertion failure must not strand the owned cleanup thread.
            released.recv_timeout(Duration::from_secs(2)).unwrap();
            None
        })?;
        setup.request_stop();
        let stopped = setup.stopped();
        let mut retiring = vec![setup];
        cleaning.recv_timeout(Duration::from_secs(2))?;
        // Cancellation retains its owner while an async timer still advances.
        assert!(
            timeout(Duration::from_millis(30), retiring[0].stopped())
                .await
                .is_err()
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
        let quit = thread::spawn(move || {
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
                    let mut command = Command::new(std::env::current_exe()?);
                    command
                        .args(["--exact", "setup::tests::setup_child_fixture", "--ignored"])
                        .env("SPEAKEASY_SETUP_TEST_ROOT", &installation_root)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null());
                    installation.output(&mut command).await?;
                    Ok(Config::default())
                },
            ))
        })?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !root.join("child.ready").exists() {
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "Setup child did not start"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let child_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("child.lock"))?;
        assert!(matches!(
            child_lock.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_archives_preserve_the_complete_runtime_layout() -> anyhow::Result<()> {
        for wrapper in ["", "nemo-speech"] {
            let directory = tempfile::tempdir()?;
            let runtime = directory.path().join(wrapper);
            fs::create_dir_all(runtime.join("bin"))?;
            fs::create_dir_all(runtime.join("lib"))?;
            fs::write(runtime.join(EXECUTABLE), b"engine fixture")?;
            fs::write(runtime.join("lib/runtime.so"), b"dependency fixture")?;
            normalize_linux_archive(directory.path())?;
            assert_eq!(
                fs::read(directory.path().join(EXECUTABLE))?,
                b"engine fixture"
            );
            assert_eq!(
                fs::read(directory.path().join("lib/runtime.so"))?,
                b"dependency fixture"
            );
            if !wrapper.is_empty() {
                assert!(!runtime.exists());
            }
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_archives_reject_an_ambiguous_runtime_layout() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        fs::create_dir_all(directory.path().join("first/bin"))?;
        fs::create_dir_all(directory.path().join("second/bin"))?;
        for wrapper in ["first", "second"] {
            fs::write(directory.path().join(wrapper).join(EXECUTABLE), b"fixture")?;
        }
        let failure = normalize_linux_archive(directory.path())
            .err()
            .context("An ambiguous engine archive was accepted")?;
        assert!(failure.to_string().contains("unexpected layout"));
        assert!(!directory.path().join(EXECUTABLE).exists());
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
            _lock: Installation::lock(root, &progress).await?,
            child: None,
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

    #[test]
    #[ignore = "Owned subprocess fixture for setup cancellation tests"]
    fn setup_child_fixture() -> anyhow::Result<()> {
        let root = PathBuf::from(
            std::env::var_os("SPEAKEASY_SETUP_TEST_ROOT").context("Missing fixture root")?,
        );
        let lock = File::create(root.join("child.lock"))?;
        lock.lock()?;
        fs::write(root.join("child.ready"), b"")?;
        thread::sleep(Duration::from_secs(60));
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_stops_the_child_before_another_setup_owns_the_directory()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().to_path_buf();
        fs::write(root.join("model.gguf.part"), &BODY[..20])?;
        let (progress, _) = watch::channel(Progress::default());
        let (cancel, cancelled) = async_channel::bounded::<()>(1);
        let first_root = root.clone();
        let first = tokio::spawn(async move {
            Installation::run(&first_root, &progress, &cancelled, async |installation| {
                let mut command = Command::new(std::env::current_exe()?);
                command
                    .args(["--exact", "setup::tests::setup_child_fixture", "--ignored"])
                    .env("SPEAKEASY_SETUP_TEST_ROOT", &first_root)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                // Require explicit termination and reaping by the owner;
                // command drop must not hide a missing cleanup step.
                installation.output(&mut command).await
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
            Err(std::fs::TryLockError::WouldBlock)
        ));

        let (progress, mut updates) = watch::channel(Progress::default());
        let (_second_cancel, second_cancelled) = async_channel::bounded::<()>(1);
        let second_root = root.clone();
        let second = tokio::spawn(async move {
            Installation::run(&second_root, &progress, &second_cancelled, async |_| {
                // The process holds this lock until it exits. A retry must not
                // enter its directory while that process can still write there.
                child_lock.try_lock()?;
                assert_eq!(fs::read(second_root.join("model.gguf.part"))?, &BODY[..20]);
                Ok(())
            })
            .await
        });
        timeout(Duration::from_secs(5), updates.changed()).await??;
        assert_eq!(updates.borrow().step, "Waiting for another setup to finish");
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
        let (cancel, cancelled) = async_channel::bounded::<()>(1);
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
            Err(std::fs::TryLockError::WouldBlock)
        ));
        Ok(())
    }

    /// Serves `BODY` on loopback and honors `Range: bytes=N-`. Returns the
    /// base URL and the range start each request asked for.
    async fn serve() -> anyhow::Result<(String, async_channel::Receiver<Option<u64>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let (tx, rx) = async_channel::unbounded();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if respond(stream, &tx).await.is_err() {
                    break;
                }
            }
        });
        Ok((url, rx))
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
        use std::fmt::Write as _;

        Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut text, byte| {
                write!(text, "{byte:02x}").unwrap();
                text
            })
    }

    #[tokio::test]
    async fn fetch_promotes_a_verified_complete_partial_without_a_request() -> anyhow::Result<()> {
        let (base, requests) = serve().await?;
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
        let (base, requests) = serve().await?;
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
        let (cancel, cancelled) = async_channel::bounded::<()>(1);
        let cancel_when_verifying = async {
            updates
                .wait_for(|progress| {
                    progress.step == "Verifying downloaded data"
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
        let (base, requests) = serve().await?;
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
        let path = fetch(&Client::new(), &download, directory.path(), "", &progress).await?;
        assert_eq!(requests.recv().await?, Some(20));
        assert_eq!(fs::read(&path)?, BODY);
        assert!(!directory.path().join("model.gguf.part").exists());
        assert_eq!(progress.borrow().done, BODY.len() as u64);
        Ok(())
    }

    #[tokio::test]
    async fn fetch_discards_a_download_that_fails_its_checksum() -> anyhow::Result<()> {
        let (base, _) = serve().await?;
        let url = format!("{base}/model.gguf");
        let sha = sha256(b"a different model");
        let download = Download {
            url: &url,
            size: BODY.len() as u64,
            sha256: &sha,
        };
        let directory = tempfile::tempdir()?;
        let (progress, _) = watch::channel(Progress::default());
        let result = fetch(&Client::new(), &download, directory.path(), "", &progress).await;
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

    // The real sources and engine, installed into a temporary directory. Uses
    // public fixture audio; never opens a microphone, hook, or clipboard.
    #[tokio::test]
    #[ignore = "Downloads about 0.8 GB; requires SPEAKEASY_FIXTURE_WAV (whisper.cpp samples/jfk.wav)"]
    async fn install_chooses_an_engine_that_recognizes_fixture_speech() -> anyhow::Result<()> {
        let wav = fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
        let root = tempfile::tempdir()?;
        let (progress, _) = watch::channel(Progress::default());
        let (_cancel, cancelled) = async_channel::bounded::<()>(1);
        let config = Installation::run(root.path(), &progress, &cancelled, async |installation| {
            install(root.path(), &progress, installation).await
        })
        .await
        .context("Setup was cancelled")??;
        eprintln!(
            "Setup chose {} with GPU {}",
            config.engine_executable.display(),
            config.use_gpu
        );
        let (_cancel, cancelled) = watch::channel(false);
        let mut worker = crate::local_speech::LocalSpeech::start(config, cancelled).await?;
        let result = worker.transcribe(wav, "en").await;
        worker.stop().await;
        // Never include the recognized text in failure output.
        assert!(
            result?.to_lowercase().contains("ask not what your country"),
            "Fixture recognition did not contain the expected phrase"
        );
        Ok(())
    }

    #[test]
    fn only_a_working_accelerator_with_enough_memory_selects_the_gpu() {
        let report = |available: bool, kind: &str, memory: u64| {
            serde_json::json!({
                "accelerator_available": available,
                "driver_runtime_compatible": true,
                "devices": [{ "type": kind, "memory_total": memory }],
            })
        };
        assert!(discrete_gpu(&report(true, "gpu", 25_756_696_576)));
        assert!(!discrete_gpu(&report(true, "gpu", 4_294_967_296)));
        assert!(!discrete_gpu(&report(
            true,
            "integrated-gpu",
            51_061_784_576
        )));
        assert!(!discrete_gpu(&report(false, "gpu", 25_756_696_576)));
    }
}
