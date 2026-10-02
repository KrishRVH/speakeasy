//! One owned local speech engine process, contained and reaped on every exit path, whose output
//! never reaches diagnostics.
//!
//! Parakeet runs in this executable's own helper mode, holding the model through the configured
//! engine installation's C library. Whisper, and a Parakeet installation without that library,
//! run the engine's HTTP server.

mod helper;
mod server;

use std::time::Duration;

use anyhow::{Context, bail};
use speakeasy_platform::ProcessGroup;
use tokio::{process::Child, sync::watch};

pub use self::helper::{HELPER_FLAG, run_helper};
use self::{helper::Helper, server::Server};
use crate::{
    child::kill_and_reap,
    config::{Config, Engine},
    ports::{STARTUP_CANCELLED, Speech},
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
// CPU inference of a five-minute recording can take minutes; Escape and bounded recovery end real
// waits long before this.
const INFERENCE_TIMEOUT: Duration = Duration::from_mins(30);
const MAX_TEXT_BYTES: usize = 1024 * 1024;

pub(crate) enum LocalSpeech {
    Helper(Helper),
    Server(Server),
}

impl LocalSpeech {
    pub(crate) async fn start(
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        match helper::library(&config) {
            Some(library) if config.engine == Engine::Parakeet => {
                Helper::start(config, library, cancelled)
                    .await
                    .map(Self::Helper)
            },
            _ => Server::start(config, cancelled).await.map(Self::Server),
        }
    }
}

impl Speech for LocalSpeech {
    async fn transcribe(&self, wav: Vec<u8>, language: &str) -> anyhow::Result<String> {
        match self {
            Self::Helper(helper) => helper.transcribe(wav, language).await,
            Self::Server(server) => server.transcribe(wav, language).await,
        }
    }

    async fn probe_with_silence(&self) -> anyhow::Result<()> {
        match self {
            Self::Helper(helper) => helper.probe_with_silence().await,
            Self::Server(server) => server.probe_with_silence().await,
        }
    }

    async fn stop(&mut self) {
        match self {
            Self::Helper(helper) => helper.stop().await,
            Self::Server(server) => server.stop().await,
        }
    }
}

/// Loading weights does not prepare every GPU kernel, so GPU configurations transcribe silence
/// before reporting ready.
struct Warmup {
    timed_out: &'static str,
    failed: &'static str,
}

impl Warmup {
    fn required(config: &Config) -> Option<Self> {
        config.use_gpu.then_some(Self {
            timed_out: "Local GPU warmup timed out. Check the selected engine and GPU dependencies.",
            failed: "Local GPU warmup failed. Check the selected engine and GPU dependencies.",
        })
    }
}

/// Takes termination authority over a freshly spawned engine, or reaps it when that fails.
async fn contain(child: &mut Child) -> anyhow::Result<ProcessGroup> {
    let pid = child.id().context("Worker exited before containment")?;
    match ProcessGroup::attach(pid) {
        Ok(group) => Ok(group),
        Err(error) => {
            kill_and_reap(child).await;
            Err(error)
        },
    }
}

async fn unless_cancelled<T>(
    cancelled: &mut watch::Receiver<bool>,
    work: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::select! {
        biased;
        _ = cancelled.changed() => bail!(STARTUP_CANCELLED),
        result = work => result,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[tokio::test]
    #[ignore = "Requires SPEAKEASY_FIXTURE_CONFIG and SPEAKEASY_FIXTURE_WAV (whisper.cpp samples/jfk.wav)"]
    async fn local_worker_recognizes_fixture_and_stops() -> anyhow::Result<()> {
        let config = Config::load(Path::new(&std::env::var("SPEAKEASY_FIXTURE_CONFIG")?))?;
        let wav = std::fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
        let language = config.language.clone();
        let started = std::time::Instant::now();
        let (_cancel, cancelled) = watch::channel(false);
        let mut worker = LocalSpeech::start(config, cancelled).await?;
        let startup = started.elapsed();
        let started = std::time::Instant::now();
        let result = worker.transcribe(wav, &language).await?;
        eprintln!(
            "Provider startup: {startup:?}; inference: {:?}",
            started.elapsed()
        );
        // Failure output must never include the recognized text.
        assert!(
            result.to_lowercase().contains("ask not what your country"),
            "Fixture recognition did not contain the expected phrase"
        );
        worker.stop().await;
        Ok(())
    }
}
