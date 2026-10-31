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

use super::{
    ModelState,
    ports::{Ports, STARTUP_CANCELLED, SessionId, Speech},
    recognition::{Detail, Part, Request, Source, Ticket, Transcript},
};
use crate::{
    config::Config,
    transcript::{
        self, Fillers,
        vocabulary::{Correction, Vocabulary},
    },
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
    Transcribing {
        ticket: Ticket,
        job: Job<(W, Transcription)>,
    },
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
            Self::Ready(_) | Self::Transcribing { .. } => ModelState::Ready,
        }
    }

    pub(super) const fn is_transcribing(&self) -> bool {
        matches!(self, Self::Transcribing { .. })
    }

    pub(super) fn is_streaming(&self, session: SessionId) -> bool {
        let Self::Transcribing { ticket, .. } = self else {
            return false;
        };
        ticket.session == session && ticket.part == Part::Stream
    }

    /// Pends unless a job is running; resolves once it finishes, leaving the worker unavailable
    /// until the owner stores the outcome. Cancel-safe: dropped early, it leaves the job running.
    pub(super) async fn completed(&mut self) -> Completion<W> {
        let completion = match self {
            Self::Loading(job) => Completion::Loaded(joined((&mut job.task).await)),
            Self::Transcribing { ticket, job } => match joined((&mut job.task).await) {
                Ok((worker, Transcription::Heard(transcript))) => {
                    Completion::Transcribed(*ticket, Ok((worker, transcript)))
                },
                // Only a job moved out of observation is cancelled; its worker is sound anyway.
                Ok((worker, Transcription::Cancelled)) => Completion::Loaded(Ok(worker)),
                Ok((worker, Transcription::Abandoned)) => Completion::Abandoned(*ticket, worker),
                Err(error) => Completion::Transcribed(*ticket, Err(error)),
            },
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
        let streaming =
            matches!(self, Self::Transcribing { ticket, .. } if ticket.part == Part::Stream);
        let previous = self.take_cancelled();
        // Engines serialize requests, and an abandoned CPU inference outlasts the probe bound.
        let probe = config.use_gpu || streaming;
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

    /// Pulls the next request only while ready, moving its audio into the owned worker job.
    pub(super) fn transcribe(&mut self, config: &Config, next: impl FnOnce() -> Option<Request>) {
        let mut worker = match mem::take(self) {
            Self::Ready(worker) => worker,
            other => {
                *self = other;
                return;
            },
        };
        let Some(request) = next() else {
            *self = Self::Ready(worker);
            return;
        };
        let language = request
            .language
            .as_deref()
            .unwrap_or(&config.language)
            .to_owned();
        let fillers = config.fillers(&language);
        let corrections = config.vocabulary.clone();
        let terms = config.recognition_terms.clone();
        *self = Self::Transcribing {
            ticket: request.ticket,
            job: Job::spawn(|mut cancelled| async move {
                let heard = async {
                    match request.audio {
                        Source::Wav(mut audio) => transcript_of(
                            &mut worker,
                            mem::take(&mut audio.wav),
                            &language,
                            request.detail,
                            fillers,
                            &corrections,
                            &terms,
                        )
                        .await
                        .map(Transcription::Heard),
                        Source::Stream(audio) => {
                            let text = worker.transcribe_stream(audio, &language, &terms).await?;
                            Ok(text
                                .map(Transcript::Text)
                                .map_or(Transcription::Abandoned, Transcription::Heard))
                        },
                    }
                };
                let transcription = tokio::select! {
                    biased;
                    _ = cancelled.changed() => Ok(Transcription::Cancelled),
                    result = heard => result,
                };
                match transcription {
                    Ok(transcription) => Ok((worker, transcription)),
                    Err(error) => {
                        worker.stop().await;
                        Err(error)
                    },
                }
            }),
        };
    }

    fn request_stop(&self) {
        match self {
            Self::Loading(job) => job.cancel(),
            Self::Transcribing { job, .. } => job.cancel(),
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
        if !*cancelled.borrow()
            && (transcribed || (probe && responds(&mut worker, cancelled).await))
        {
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
            Self::Transcribing { job, .. } => {
                job.task.await.ok()?.ok().map(|(worker, transcription)| {
                    (
                        worker,
                        matches!(
                            transcription,
                            Transcription::Heard(_) | Transcription::Abandoned
                        ),
                    )
                })
            },
            Self::Unavailable => None,
        }
    }
}

pub(super) enum Completion<W> {
    /// The stream ended without final text; reuse the worker for whole-recording fallback.
    Abandoned(Ticket, W),
    Loaded(anyhow::Result<W>),
    Transcribed(Ticket, anyhow::Result<(W, Transcript)>),
}

pub(super) enum Transcription {
    /// A stream ended without final text; the worker remains reusable.
    Abandoned,
    Heard(Transcript),
    /// The job was detached from observation and its request was cancelled.
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
    result.unwrap_or_else(|_| Err(anyhow!("Transcription stopped unexpectedly. Try again.")))
}

/// Returns raw dictation text for whole-session cleanup, or cleaned timed Show narration.
async fn transcript_of<W: Speech>(
    worker: &mut W,
    wav: Vec<u8>,
    language: &str,
    detail: Detail,
    fillers: Fillers,
    corrections: &[Correction],
    terms: &crate::recognition_terms::Terms,
) -> anyhow::Result<Transcript> {
    Ok(match detail {
        Detail::Text => Transcript::Text(worker.transcribe(wav, language, terms).await?),
        Detail::Words => {
            let narration = worker.transcribe_words(wav, language, terms).await?;
            Transcript::Words(
                Vocabulary::new(corrections).narration(transcript::for_show(narration, fillers)),
            )
        },
    })
}

/// Whether a silent request completes within the recovery bound, unless cancellation comes first.
async fn responds<W: Speech>(worker: &mut W, cancelled: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        biased;
        _ = cancelled.changed() => false,
        result = timeout(RECOVERY_PROBE, worker.probe_with_silence()) => matches!(result, Ok(Ok(()))),
    }
}
