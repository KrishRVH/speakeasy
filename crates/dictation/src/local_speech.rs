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
use speakeasy_platform::{ProcessGroup, speech::Failure};
use tokio::{process::Child, sync::watch, time::timeout};

pub use self::helper::{HELPER_FLAG, run_helper};
use self::{helper::Helper, server::Server};
use crate::{
    audio,
    child::kill_and_reap,
    config::{Config, Engine},
    ports::{STARTUP_CANCELLED, Speech},
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
// CPU inference of a five-minute recording can take minutes; cancellation and the bound on
// abandoned transcriptions end real waits long before this.
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
        let Some(library) = (config.engine == Engine::Parakeet)
            .then(|| helper::library(&config))
            .flatten()
        else {
            return Server::start(config, cancelled).await.map(Self::Server);
        };
        match Helper::start(&config, library, cancelled.clone()).await {
            Ok(helper) => Ok(Self::Helper(helper)),
            // A library this process cannot load, such as one a hardened runtime refuses, still
            // leaves the installation's own server.
            Err(error) if error.downcast_ref::<Failure>() == Some(&Failure::Library) => {
                Server::start(config, cancelled).await.map(Self::Server)
            },
            Err(error) => Err(error),
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

    async fn stop(&mut self) {
        match self {
            Self::Helper(helper) => helper.stop().await,
            Self::Server(server) => server.stop().await,
        }
    }
}

/// Loading weights does not prepare every GPU kernel, so a GPU engine transcribes a second of
/// silence before reporting ready; completing it also proves the model responds, which a health
/// check cannot.
async fn warm_up(
    speech: &impl Speech,
    cancelled: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let silence = audio::silent_wav(16_000, Duration::from_secs(1))?;
    unless_cancelled(cancelled, async {
        timeout(STARTUP_TIMEOUT, speech.transcribe(silence, "en"))
            .await
            .context("Local GPU warmup timed out. Check the selected engine and GPU dependencies.")?
            .context("Local GPU warmup failed. Check the selected engine and GPU dependencies.")
    })
    .await?;
    Ok(())
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
    #[ignore = "Requires SPEAKEASY_FIXTURE_CONFIG, SPEAKEASY_FIXTURE_WAV (whisper.cpp samples/jfk.wav), and for Parakeet SPEAKEASY_FIXTURE_HELPER"]
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
            "Engine startup: {startup:?}; inference: {:?}",
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
