//! One warm worker, or one job that owns it. Replacement moves the previous job out of the
//! observation path, then awaits its cleanup before loading. Errors reap the owned worker;
//! unwinding remains an emergency kill-on-drop.

use std::{future::pending, mem, time::Duration};

use anyhow::{anyhow, ensure};
use tokio::{
    sync::watch,
    task::{JoinError, JoinHandle},
    time::timeout,
};

use super::ModelState;
use crate::{
    config::Config,
    ports::{Ports, STARTUP_CANCELLED, Speech},
};

/// How long a recovering worker may take to answer a silent request.
const RECOVERY_PROBE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) enum Worker<W> {
    #[default]
    Unavailable,
    /// Starting a worker, or recovering the one a cancelled transcription left behind.
    Loading(Job<W>),
    Ready(W),
    Transcribing(Job<(W, Transcript)>),
}

impl<W: Speech> Worker<W> {
    pub(super) fn load<P: Ports<Speech = W>>(ports: &P, config: &Config) -> Self {
        Self::Loading(Job::spawn(|cancelled| {
            ports.load(config.clone(), cancelled)
        }))
    }

    pub(super) const fn model(&self) -> ModelState {
        match self {
            Self::Unavailable => ModelState::Unavailable,
            Self::Loading(_) => ModelState::Loading,
            Self::Ready(_) | Self::Transcribing(_) => ModelState::Ready,
        }
    }

    pub(super) const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    pub(super) const fn is_transcribing(&self) -> bool {
        matches!(self, Self::Transcribing(_))
    }

    /// Pends unless a job is running; resolves once it finishes, leaving the worker unavailable
    /// until the owner stores the outcome. Cancel-safe: dropped early, it leaves the job running.
    pub(super) async fn completed(&mut self) -> Completion<W> {
        let completion = match self {
            Self::Loading(job) => Completion::Loaded(joined((&mut job.task).await)),
            Self::Transcribing(job) => Completion::Transcribed(joined((&mut job.task).await)),
            Self::Unavailable | Self::Ready(_) => pending().await,
        };
        *self = Self::Unavailable;
        completion
    }

    /// Starts loading when no worker exists; returns whether a load began.
    pub(super) fn revive<P: Ports<Speech = W>>(&mut self, ports: &P, config: &Config) -> bool {
        let unavailable = matches!(self, Self::Unavailable);
        if unavailable {
            *self = Self::load(ports, config);
        }
        unavailable
    }

    /// Stops the current worker and loads a fresh one once it has exited.
    pub(super) fn reload<P: Ports<Speech = W>>(&mut self, ports: &P, config: &Config) {
        let previous = self.take_cancelled();
        *self = Self::Loading(Job::spawn(|cancelled| {
            let load = ports.load(config.clone(), cancelled.clone());
            async move {
                previous.stop().await;
                ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
                load.await
            }
        }));
    }

    /// Keeps the worker of a cancelled transcription when it still responds; otherwise stops it and
    /// loads a replacement.
    pub(super) fn recover<P: Ports<Speech = W>>(&mut self, ports: &P, config: &Config) {
        let previous = self.take_cancelled();
        // Engines serialize requests, and an abandoned CPU inference outlasts the probe bound.
        let probe = config.use_gpu;
        *self = Self::Loading(Job::spawn(|mut cancelled| {
            let load = ports.load(config.clone(), cancelled.clone());
            async move {
                if let Some(worker) = previous.into_responsive(probe, &mut cancelled).await {
                    return Ok(worker);
                }
                ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
                load.await
            }
        }));
    }

    /// Starts transcribing on a ready worker, or hands the audio back.
    pub(super) fn transcribe(&mut self, wav: Vec<u8>, language: &str) -> Result<(), Vec<u8>> {
        let mut worker = match mem::take(self) {
            Self::Ready(worker) => worker,
            other => {
                *self = other;
                return Err(wav);
            },
        };
        let language = language.to_owned();
        *self = Self::Transcribing(Job::spawn(|mut cancelled| async move {
            let transcript = tokio::select! {
                biased;
                _ = cancelled.changed() => Ok(Transcript::Cancelled),
                result = worker.transcribe(wav, &language) => result.map(Transcript::Text),
            };
            match transcript {
                Ok(transcript) => Ok((worker, transcript)),
                Err(error) => {
                    worker.stop().await;
                    Err(error)
                },
            }
        }));
        Ok(())
    }

    pub(super) fn request_stop(&self) {
        match self {
            Self::Loading(job) => job.cancel(),
            Self::Transcribing(job) => job.cancel(),
            Self::Unavailable | Self::Ready(_) => {},
        }
    }

    /// Cancels any job and resolves once its worker has stopped.
    pub(super) async fn stop(self) {
        self.request_stop();
        if let Some((mut worker, _)) = self.into_worker().await {
            worker.stop().await;
        }
    }

    /// Returns the worker if its transcription completed or, when probing is allowed, it answers a
    /// silent request in time; otherwise, or once cancelled, stops it.
    async fn into_responsive(
        self,
        probe: bool,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Option<W> {
        let (mut worker, transcribed) = self.into_worker().await?;
        if !*cancelled.borrow() && (transcribed || (probe && responds(&worker, cancelled).await)) {
            return Some(worker);
        }
        worker.stop().await;
        None
    }

    /// Takes the current state out of the observation path and cancels its job.
    fn take_cancelled(&mut self) -> Self {
        let previous = mem::take(self);
        previous.request_stop();
        previous
    }

    /// Waits out any job and returns its worker, noting whether it just completed a transcript.
    async fn into_worker(self) -> Option<(W, bool)> {
        match self {
            Self::Ready(worker) => Some((worker, false)),
            Self::Loading(job) => job.task.await.ok()?.ok().map(|worker| (worker, false)),
            Self::Transcribing(job) => job
                .task
                .await
                .ok()?
                .ok()
                .map(|(worker, transcript)| (worker, matches!(transcript, Transcript::Text(_)))),
            Self::Unavailable => None,
        }
    }
}

pub(super) enum Completion<W> {
    Loaded(anyhow::Result<W>),
    Transcribed(anyhow::Result<(W, Transcript)>),
}

pub(super) enum Transcript {
    Text(String),
    Cancelled,
}

pub(super) struct Job<T> {
    cancellation: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<T>>,
}

impl<T: Send + 'static> Job<T> {
    fn spawn<F>(work: impl FnOnce(watch::Receiver<bool>) -> F) -> Self
    where
        F: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let (cancellation, cancelled) = watch::channel(false);
        Self {
            cancellation,
            task: tokio::spawn(work(cancelled)),
        }
    }

    fn cancel(&self) {
        self.cancellation.send_replace(true);
    }
}

/// A failed join reports a fixed message, since a panic payload could carry private text.
fn joined<T>(result: Result<anyhow::Result<T>, JoinError>) -> anyhow::Result<T> {
    result.unwrap_or_else(|_| {
        Err(anyhow!(
            "Transcription worker stopped unexpectedly. Try again."
        ))
    })
}

/// Whether a silent request completes within the recovery bound, unless cancellation comes first.
async fn responds<W: Speech>(worker: &W, cancelled: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        biased;
        _ = cancelled.changed() => false,
        result = timeout(RECOVERY_PROBE, worker.probe_with_silence()) => matches!(result, Ok(Ok(()))),
    }
}
