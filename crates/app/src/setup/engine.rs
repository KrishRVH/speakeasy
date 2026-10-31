//! Engine choice: each platform's pinned NeMo-Speech.cpp builds and the order setup tries them in.
//! A GPU build is kept only once its own `doctor` report shows it runs on this machine.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

#[cfg(target_os = "macos")]
use anyhow::Context as _;
#[cfg(not(target_os = "macos"))]
use anyhow::bail;
use reqwest::Client;
use tokio::{sync::watch, time::timeout};

use super::{
    Installation, Progress,
    archive::{EXECUTABLE, unpack},
    download::Download,
};
use crate::child::hidden_command;

#[cfg(target_os = "windows")]
const CUDA: Download<'static> = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/nemo-speech-0.2.0-windows-x86_64-cuda.zip",
    size: 102_183_465,
    sha256: "9ffe5e842c39a7d3211f3b2c3b3aa1bca8c0b2455d96d961ed5f22ec5108f98d",
};

#[cfg(target_os = "windows")]
const VULKAN: Download<'static> = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/nemo-speech-0.2.0-windows-x86_64-vulkan.zip",
    size: 19_302_464,
    sha256: "edf15a04ba98740aa0ab75ae4681ac7ad0a488b22b35ce2cbebd10590bb1c1e1",
};

#[cfg(target_os = "windows")]
const CPU: Download<'static> = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/nemo-speech-0.2.0-windows-x86_64-cpu.zip",
    size: 5_528_418,
    sha256: "82c80451086e86194aba9af19f800da77cd66d1c28d993d0efceffd6cdb49bb8",
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CUDA: Download<'static> = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/nemo-speech-0.2.0-linux-x86_64-cuda.tar.gz",
    size: 105_010_050,
    sha256: "aef573fcc6c2c759401d2ed03e5ca9e401ed23b0e38cb852d917080c1e2d4515",
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const VULKAN: Download<'static> = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/nemo-speech-0.2.0-linux-x86_64-vulkan.tar.gz",
    size: 16_618_142,
    sha256: "c8d0ed5b9ac5fb6d5c460015ba38ea217415c44edc1e6d49120646781009aee6",
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CPU: Download<'static> = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/nemo-speech-0.2.0-linux-x86_64-cpu.tar.gz",
    size: 6_683_477,
    sha256: "f396057150f1b774935c7414fd32ebc04195a795a34120d78d8c9ecefa1b7507",
};

#[cfg(target_os = "macos")]
const METAL: Download<'static> = Download {
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/nemo-speech-0.2.0-macos-aarch64-metal.tar.gz",
    size: 5_031_510,
    sha256: "5cb02ba7c04f0b5585ce5cde9c830be5f0b7dfb4c83083c500109c381b4f2da9",
};

/// The engine build setup chose, and whether it should run on the GPU. The other builds stay until
/// the model is in place, so a setup paused before then resumes without downloading them again.
pub(super) struct ChosenEngine {
    directory: PathBuf,
    pub(super) use_gpu: bool,
}

impl ChosenEngine {
    /// Other builds may still belong to the active worker. Setup never removes a usable engine.
    pub(super) fn executable(self) -> PathBuf {
        self.directory.join(EXECUTABLE)
    }
}

/// Tries CUDA with an NVIDIA driver, then Vulkan, then CPU. `LocalSpeech` still transcribes silence
/// before reporting a GPU choice, or any Linux one, ready.
#[cfg(any(
    target_os = "windows",
    all(target_os = "linux", target_arch = "x86_64")
))]
pub(super) async fn choose_engine(
    client: &Client,
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<ChosenEngine> {
    if nvidia_driver() {
        let cuda = unpack(client, &CUDA, root, progress, installation).await?;
        if doctor(&cuda, progress, installation)
            .await
            .is_some_and(|report| has_discrete_gpu(&report))
        {
            return Ok(ChosenEngine {
                directory: cuda,
                use_gpu: true,
            });
        }
    }
    let vulkan = unpack(client, &VULKAN, root, progress, installation).await?;
    if let Some(report) = doctor(&vulkan, progress, installation).await {
        return Ok(ChosenEngine {
            directory: vulkan,
            use_gpu: has_discrete_gpu(&report),
        });
    }
    let cpu = unpack(client, &CPU, root, progress, installation).await?;
    if cfg!(target_os = "linux") && doctor(&cpu, progress, installation).await.is_none() {
        bail!(
            "The downloaded engine cannot run on this Linux CPU. Choose a portable NeMo-Speech.cpp build in Settings."
        );
    }
    Ok(ChosenEngine {
        directory: cpu,
        use_gpu: false,
    })
}

#[cfg(target_os = "macos")]
pub(super) async fn choose_engine(
    client: &Client,
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<ChosenEngine> {
    let metal = unpack(client, &METAL, root, progress, installation).await?;
    let report = doctor(&metal, progress, installation)
        .await
        .context(
            "The speech engine cannot run on this Mac. Choose a compatible engine executable in Settings.",
        )?;
    Ok(ChosenEngine {
        directory: metal,
        use_gpu: is_accelerated(&report),
    })
}

#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
pub(super) async fn choose_engine(
    _: &Client,
    _: &Path,
    _: &watch::Sender<Progress>,
    _: &mut Installation,
) -> anyhow::Result<ChosenEngine> {
    bail!(
        "Automatic Linux setup requires x86_64. Choose an engine executable and model for this architecture in Settings."
    )
}

#[cfg(any(
    target_os = "windows",
    all(target_os = "linux", target_arch = "x86_64")
))]
fn nvidia_driver() -> bool {
    if cfg!(target_os = "windows") {
        super::system32().join("nvidia-smi.exe").is_file()
    } else {
        Path::new("/proc/driver/nvidia/version").is_file()
    }
}

/// Asks the build at `directory` whether it runs here, which can take up to a minute on a cold GPU.
async fn doctor(
    directory: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> Option<serde_json::Value> {
    progress.send_replace(Progress::new("Checking the speech engine on this machine"));
    let mut command = hidden_command(directory.join(EXECUTABLE));
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

#[cfg(any(
    target_os = "windows",
    all(target_os = "linux", target_arch = "x86_64"),
    test
))]
fn has_discrete_gpu(report: &serde_json::Value) -> bool {
    // After a five-minute recording, Parakeet holds about 3.8 GB of GPU memory.
    const GPU_MEMORY: u64 = 6_000_000_000;
    is_accelerated(report)
        && report["devices"].as_array().is_some_and(|devices| {
            devices.iter().any(|device| {
                device["type"] == "gpu"
                    && device["memory_total"]
                        .as_u64()
                        .is_some_and(|memory| memory >= GPU_MEMORY)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_working_accelerator_with_enough_memory_selects_the_gpu() {
        let report = |available: bool, kind: &str, memory: u64| {
            serde_json::json!({
                "accelerator_available": available,
                "driver_runtime_compatible": true,
                "devices": [{ "type": kind, "memory_total": memory }],
            })
        };
        assert!(has_discrete_gpu(&report(true, "gpu", 25_756_696_576)));
        assert!(!has_discrete_gpu(&report(true, "gpu", 4_294_967_296)));
        assert!(!has_discrete_gpu(&report(
            true,
            "integrated-gpu",
            51_061_784_576
        )));
        assert!(!has_discrete_gpu(&report(false, "gpu", 25_756_696_576)));
    }
}
