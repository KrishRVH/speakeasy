//! Automatic setup installs the NeMo-Speech.cpp build that suits this machine
//! and the Parakeet v3 model. Every download is pinned by URL, size, and
//! SHA-256, resumes after interruption, and is renamed into place only once
//! verified.
#![cfg_attr(
    not(any(target_os = "windows", target_os = "macos")),
    expect(dead_code, reason = "Linux runs the preview only")
)]

use crate::config::{Config, Engine};
use anyhow::{Context, bail};
use reqwest::{Client, StatusCode, header::RANGE};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::Duration,
};
use tokio::{process::Command, sync::watch, time::timeout};

#[derive(Clone, Default)]
pub struct Progress {
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
#[cfg(target_os = "macos")]
const METAL: Download = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-macos-aarch64-metal.tar.gz",
    size: 3_465_028,
    sha256: "f1dff4f9dd9c96214f8cb78b982812459132df8a4ad1a42409fd94de4a366244",
};

#[cfg(target_os = "windows")]
const EXECUTABLE: &str = r"bin\nemo-speech.exe";
#[cfg(not(target_os = "windows"))]
const EXECUTABLE: &str = "nemo-speech/bin/nemo-speech";

// After a five-minute recording, Parakeet holds about 3.8 GB of GPU memory.
#[cfg(any(target_os = "windows", test))]
const GPU_MEMORY: u64 = 6_000_000_000;

/// A running setup. Dropping it cancels; partial downloads resume next time.
pub struct Setup {
    pub progress: watch::Receiver<Progress>,
    pub result: async_channel::Receiver<anyhow::Result<Config>>,
    _cancel: async_channel::Sender<()>,
}

impl Setup {
    pub fn start() -> anyhow::Result<Self> {
        let (report, progress) = watch::channel(Progress::default());
        let (finish, result) = async_channel::bounded(1);
        let (cancel, cancelled) = async_channel::bounded::<()>(1);
        thread::Builder::new().name("setup".into()).spawn(move || {
            let root = root();
            let outcome = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(async {
                    tokio::select! {
                        result = install(&root, &report) => Some(result),
                        _ = cancelled.recv() => None,
                    }
                }),
                Err(error) => Some(Err(error.into())),
            };
            if let Some(result) = outcome {
                let _ = finish.send_blocking(result);
            }
        })?;
        Ok(Self {
            progress,
            result,
            _cancel: cancel,
        })
    }
}

/// Engines and models are machine-local, so they stay out of roaming settings.
fn root() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support"));
    base.unwrap_or_else(|| PathBuf::from(".")).join("speakeasy")
}

async fn install(root: &Path, progress: &watch::Sender<Progress>) -> anyhow::Result<Config> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()?;
    let (engine_executable, use_gpu) = engine(&client, root, progress).await?;
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

/// NVIDIA drivers install nvidia-smi; NeMo's own doctor then confirms that the
/// CUDA build runs here. Other discrete GPUs use Vulkan. The Vulkan build also
/// runs on the CPU, and the CPU build covers machines without a Vulkan loader.
#[cfg(target_os = "windows")]
async fn engine(
    client: &Client,
    root: &Path,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<(PathBuf, bool)> {
    if system32().join("nvidia-smi.exe").is_file() {
        let cuda = unpack(client, &CUDA, root, progress).await?;
        if doctor(&cuda)
            .await
            .is_some_and(|report| discrete_gpu(&report))
        {
            return keep(root, cuda, true);
        }
    }
    let vulkan = unpack(client, &VULKAN, root, progress).await?;
    if let Some(report) = doctor(&vulkan).await {
        return keep(root, vulkan, discrete_gpu(&report));
    }
    let cpu = unpack(client, &CPU, root, progress).await?;
    keep(root, cpu, false)
}

#[cfg(target_os = "macos")]
async fn engine(
    client: &Client,
    root: &Path,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<(PathBuf, bool)> {
    let metal = unpack(client, &METAL, root, progress).await?;
    let report = doctor(&metal)
        .await
        .context("The speech engine cannot run on this Mac.")?;
    keep(root, metal, accelerated(&report))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
async fn engine(
    _: &Client,
    _: &Path,
    _: &watch::Sender<Progress>,
) -> anyhow::Result<(PathBuf, bool)> {
    bail!("Automatic setup supports Windows and macOS.")
}

#[cfg(target_os = "windows")]
fn system32() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or("C:\\Windows".into())).join("System32")
}

/// Removes other engine builds and returns the chosen executable.
fn keep(root: &Path, directory: PathBuf, gpu: bool) -> anyhow::Result<(PathBuf, bool)> {
    for entry in fs::read_dir(root.join("engines"))?.flatten() {
        if entry.path() != directory {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    Ok((directory.join(EXECUTABLE), gpu))
}

async fn doctor(directory: &Path) -> Option<serde_json::Value> {
    let mut command = Command::new(directory.join(EXECUTABLE));
    command
        .args(["doctor", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let output = timeout(Duration::from_secs(60), command.output())
        .await
        .ok()?
        .ok()?;
    serde_json::from_slice(&output.stdout).ok()
}

fn accelerated(report: &serde_json::Value) -> bool {
    report["accelerator_available"] == true && report["driver_runtime_compatible"] == true
}

#[cfg(any(target_os = "windows", test))]
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
    let _ = fs::remove_dir_all(&staging);
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
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    if !command.status().await?.success() {
        bail!("Cannot unpack the speech engine. Check free disk space, then try again.");
    }
    let _ = fs::remove_dir_all(&directory);
    fs::rename(&staging, &directory)?;
    let _ = fs::remove_file(&file);
    Ok(directory)
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
    if done >= download.size {
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
        std::io::copy(&mut File::open(&partial)?, &mut hasher)?;
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
        done += chunk.len() as u64;
        if done > download.size {
            break;
        }
        hasher.update(&chunk);
        file.write_all(&chunk)?;
        progress.send_modify(|progress| progress.done = done);
    }
    file.sync_all()?;
    drop(file);
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if done != download.size || digest != download.sha256 {
        let _ = fs::remove_file(&partial);
        bail!("A download did not match its published checksum. Try again.");
    }
    fs::rename(&partial, &path)?;
    Ok(path)
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
        let _ = requests.send(start.map(|start| start as u64)).await;
        let head = match start {
            Some(start) => format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{}\r\n",
                BODY.len() - 1,
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
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
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
        let config = install(root.path(), &progress).await?;
        eprintln!(
            "Setup chose {} with GPU {}",
            config.engine_executable.display(),
            config.use_gpu
        );
        let mut worker = crate::local_speech::LocalSpeech::start(config).await?;
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
