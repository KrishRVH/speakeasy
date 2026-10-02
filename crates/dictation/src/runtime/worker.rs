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
    config::{Config, Engine},
    ports::{Ports, STARTUP_CANCELLED, Speech},
    transcript::{self, Fillers},
};

/// How long a GPU transcription abandoned with its session may run on before its worker is
/// replaced.
const ABANDONED_GPU_BOUND: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) enum Worker<W> {
    #[default]
    Unavailable,
    /// Starting a worker, or replacing one whose abandoned transcription could not be kept.
    Loading(Job<W>),
    /// Letting an abandoned GPU transcription finish unobserved, so its warm worker is kept.
    Settling(Job<W>),
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
            Self::Ready(_) | Self::Settling(_) | Self::Transcribing(_) => ModelState::Ready,
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
            Self::Loading(job) | Self::Settling(job) => {
                Completion::Loaded(joined((&mut job.task).await))
            },
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

    /// Moves a transcription whose session ended out of observation, discarding its result. Engines
    /// serialize requests and a GPU request ends within moments, so it runs out and the warm worker
    /// is kept without loading or probing; one that overruns the bound, or a CPU inference that
    /// could run for seconds, is cancelled and its worker replaced.
    pub(super) fn abandon<P: Ports<Speech = W>>(&mut self, ports: &P, config: &Config) {
        let previous = mem::take(self);
        let bound = if config.use_gpu {
            ABANDONED_GPU_BOUND
        } else {
            Duration::ZERO
        };
        let job = Job::spawn(|mut cancelled| {
            let load = ports.load(config.clone(), cancelled.clone());
            async move {
                if let Some(worker) = previous.settled(bound, &mut cancelled).await {
                    return Ok(worker);
                }
                ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
                load.await
            }
        });
        *self = if config.use_gpu {
            Self::Settling(job)
        } else {
            Self::Loading(job)
        };
    }

    /// Starts transcribing on a ready worker, or hands the audio back.
    pub(super) fn transcribe(&mut self, wav: Vec<u8>, config: &Config) -> Result<(), Vec<u8>> {
        let mut worker = match mem::take(self) {
            Self::Ready(worker) => worker,
            other => {
                *self = other;
                return Err(wav);
            },
        };
        let language = config.language.clone();
        // Parakeet auto-detects without returning its language, so its user-selected English
        // cleanup preference is authoritative. Whisper's explicit non-English/auto choice wins.
        let fillers = if config.remove_fillers
            && (config.engine == Engine::Parakeet || language.eq_ignore_ascii_case("en"))
        {
            Fillers::Remove
        } else {
            Fillers::Preserve
        };
        *self = Self::Transcribing(Job::spawn(|mut cancelled| async move {
            let transcript = tokio::select! {
                biased;
                _ = cancelled.changed() => Ok(Transcript::Cancelled),
                result = worker.transcribe(wav, &language) => {
                    result.map(|text| Transcript::Text(transcript::for_insertion(&text, fillers)))
                },
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
            Self::Loading(job) | Self::Settling(job) => job.cancel(),
            Self::Transcribing(job) => job.cancel(),
            Self::Unavailable | Self::Ready(_) => {},
        }
    }

    /// Cancels any job and resolves once its worker has stopped.
    pub(super) async fn stop(self) {
        self.request_stop();
        if let Some(mut worker) = self.into_worker().await {
            worker.stop().await;
        }
    }

    /// Returns the worker once its transcription finishes within `bound`; otherwise, or once
    /// cancelled, cancels the transcription and stops the worker.
    async fn settled(self, bound: Duration, cancelled: &mut watch::Receiver<bool>) -> Option<W> {
        let Self::Transcribing(mut job) = self else {
            return self.into_worker().await;
        };
        let finished = tokio::select! {
            biased;
            _ = cancelled.changed() => None,
            finished = timeout(bound, &mut job.task) => finished.ok(),
        };
        let mut worker = match finished {
            Some(Ok(Ok((worker, _)))) if !*cancelled.borrow() => return Some(worker),
            Some(Ok(Ok((worker, _)))) => worker,
            // A failed transcription has already stopped its worker.
            Some(_) => return None,
            None => {
                job.cancel();
                job.task.await.ok()?.ok()?.0
            },
        };
        worker.stop().await;
        None
    }

    /// Takes the current state out of the observation path and cancels its job.
    fn take_cancelled(&mut self) -> Self {
        let previous = mem::take(self);
        previous.request_stop();
        previous
    }

    /// Waits out any job and returns its worker.
    async fn into_worker(self) -> Option<W> {
        match self {
            Self::Ready(worker) => Some(worker),
            Self::Loading(job) | Self::Settling(job) => job.task.await.ok()?.ok(),
            Self::Transcribing(job) => job.task.await.ok()?.ok().map(|(worker, _)| worker),
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
