use std::{future::pending, iter, mem, time::Duration};

use anyhow::{anyhow, ensure};
use speakeasy_core::machine::{Detail, Request, Source, Ticket, Transcript};
use speakeasy_show::Narration;
use tokio::{
    sync::watch,
    task::{JoinError, JoinHandle},
    time::timeout,
};
use zeroize::Zeroizing;

use super::{
    native::{Clip, Native},
    ports::{Ports, STARTUP_CANCELLED, Speech},
};
use crate::{
    config::{Config, Engine},
    recognition_terms::Terms,
    transcript::{
        self, Fillers, credits,
        vocabulary::{Correction, Vocabulary},
    },
};

const RECOVERY_PROBE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) enum Worker<W> {
    #[default]
    Unavailable,
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

    pub(super) async fn completed(&mut self) -> Completion<W> {
        let completion = match self {
            Self::Loading(job) => Completion::Loaded(joined(
                (&mut job.task).await,
                "Loading the local model stopped unexpectedly. Try again.",
            )),
            Self::Transcribing { ticket, job } => match joined(
                (&mut job.task).await,
                "Transcription stopped unexpectedly. Try again.",
            ) {
                Ok((worker, Transcription::Heard(transcript))) => {
                    Completion::Transcribed(*ticket, Ok((worker, transcript)))
                },
                Ok((worker, Transcription::Cancelled)) => Completion::Loaded(Ok(worker)),
                Ok((worker, Transcription::Abandoned)) => Completion::Abandoned(*ticket, worker),
                Err(error) => Completion::Transcribed(*ticket, Err(error)),
            },
            Self::Unavailable | Self::Ready(_) => pending().await,
        };
        *self = Self::Unavailable;
        completion
    }

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

    pub(super) fn recover<P: Ports<Speech = W>>(
        &mut self,
        ports: &P,
        config: &Config,
        probe: bool,
    ) {
        let previous = self.take_cancelled();
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

    pub(super) fn transcribe(&mut self, config: &Config, request: Request<Native>) {
        let mut worker = match mem::take(self) {
            Self::Ready(worker) => worker,
            other => {
                *self = other;
                return;
            },
        };
        let language = match &request.source {
            Source::Audio(clip) => clip.language.as_deref(),
            Source::Live(_) => None,
        }
        .unwrap_or(&config.language)
        .to_owned();
        let cleanup = Cleanup {
            engine: config.engine,
            fillers: config.fillers(&language),
            corrections: config.vocabulary.clone(),
        };
        let terms = config.recognition_terms.clone();
        *self = Self::Transcribing {
            ticket: request.ticket,
            job: Job::spawn(|mut cancelled| async move {
                let heard = async {
                    match request.source {
                        Source::Audio(clip) => transcript_of(
                            &mut worker,
                            clip,
                            &language,
                            request.detail,
                            &cleanup,
                            &terms,
                        )
                        .await
                        .map(Transcription::Heard),
                        Source::Live(audio) => {
                            let text = worker.transcribe_stream(audio, &language, &terms).await?;
                            Ok(text
                                .map(|text| Transcript::Text(Zeroizing::new(text)))
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

    pub(super) async fn stop(self) {
        self.request_stop();
        if let Some((mut worker, _)) = self.into_worker().await {
            worker.stop().await;
        }
    }

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

    fn take_cancelled(&mut self) -> Self {
        let previous = mem::take(self);
        previous.request_stop();
        previous
    }

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
    Abandoned(Ticket, W),
    Loaded(anyhow::Result<W>),
    Transcribed(Ticket, anyhow::Result<(W, Transcript<Native>)>),
}

pub(super) enum Transcription {
    Abandoned,
    Heard(Transcript<Native>),
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

fn joined<T>(
    result: Result<anyhow::Result<T>, JoinError>,
    message: &'static str,
) -> anyhow::Result<T> {
    result.unwrap_or_else(|_private_panic| Err(anyhow!(message)))
}

struct Cleanup {
    engine: Engine,
    fillers: Fillers,
    corrections: Vec<Correction>,
}

impl Cleanup {
    fn invented<'a>(
        &self,
        heard: impl Iterator<Item = &'a str> + Clone,
        audible: Duration,
    ) -> bool {
        self.engine == Engine::Whisper && credits::invented(heard, audible)
    }

    fn narration(&self, narration: Narration) -> Narration {
        Vocabulary::new(&self.corrections).narration(transcript::for_show(narration, self.fillers))
    }
}

async fn transcript_of<W: Speech>(
    worker: &mut W,
    mut clip: Clip,
    language: &str,
    detail: Detail,
    cleanup: &Cleanup,
    terms: &Terms,
) -> anyhow::Result<Transcript<Native>> {
    let wav = mem::take(&mut *clip.wav);
    Ok(match detail {
        Detail::Text => {
            let text = Zeroizing::new(worker.transcribe(wav, language, terms).await?);
            Transcript::Text(
                if cleanup.invented(iter::once(text.as_str()), clip.audible) {
                    Zeroizing::default()
                } else {
                    text
                },
            )
        },
        Detail::Words => {
            let narration = worker.transcribe_words(wav, language, terms).await?;
            let invented = match &narration {
                Narration::Text(text) => cleanup.invented(iter::once(text.as_str()), clip.audible),
                Narration::Words(words) => {
                    cleanup.invented(words.iter().map(|word| word.text.as_str()), clip.audible)
                },
            };
            Transcript::Words(if invented {
                Narration::default()
            } else {
                cleanup.narration(narration)
            })
        },
    })
}

async fn responds<W: Speech>(worker: &mut W, cancelled: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        biased;
        _ = cancelled.changed() => false,
        result = timeout(RECOVERY_PROBE, worker.probe_with_silence()) => matches!(result, Ok(Ok(()))),
    }
}
