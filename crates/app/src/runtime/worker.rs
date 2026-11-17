use std::{future::pending, iter, mem, time::Duration};

use anyhow::{anyhow, ensure};
use speakeasy_core::machine::{Completion, Detail, Request, Ticket, Transcript};
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

const LOAD_STOPPED: &str = "Loading the local model stopped unexpectedly. Try again.";

const TRANSCRIPTION_STOPPED: &str = "Transcription stopped unexpectedly. Try again.";

pub(super) struct Worker<W> {
    state: State<W>,
}

#[derive(Default)]
enum State<W> {
    #[default]
    Unavailable,
    Loading(Job<W>),
    Ready(W),
    Transcribing {
        ticket: Ticket,
        job: Job<(W, Transcription)>,
    },
}

impl<W> Default for Worker<W> {
    fn default() -> Self {
        Self {
            state: State::Unavailable,
        }
    }
}

impl<W: Speech> Worker<W> {
    pub(super) fn load<P: Ports<Speech = W>>(ports: &P, config: &Config) -> Self {
        Self {
            state: State::Loading(Job::spawn(|cancelled| {
                ports.load(config.clone(), cancelled)
            })),
        }
    }

    pub(super) async fn completed(&mut self) -> Completion<Native> {
        let (state, completion) = match &mut self.state {
            State::Loading(job) => match joined((&mut job.task).await, LOAD_STOPPED) {
                Ok(worker) => (State::Ready(worker), Completion::Loaded(Ok(()))),
                Err(error) => (
                    State::Unavailable,
                    Completion::Loaded(Err(error.to_string())),
                ),
            },
            State::Transcribing { ticket, job } => {
                let ticket = *ticket;
                match joined((&mut job.task).await, TRANSCRIPTION_STOPPED) {
                    Ok((worker, transcription)) => {
                        (State::Ready(worker), transcription.completion(ticket))
                    },
                    Err(error) => (
                        State::Unavailable,
                        Completion::Transcribed(ticket, Err(error.to_string())),
                    ),
                }
            },
            State::Unavailable | State::Ready(_) => pending().await,
        };
        self.state = state;
        completion
    }

    pub(super) fn reload<P: Ports<Speech = W>>(&mut self, ports: &P, config: &Config) {
        let previous = self.take_cancelled();
        self.state = State::Loading(Job::spawn(|cancelled| {
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
        self.state = State::Loading(Job::spawn(|mut cancelled| {
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
        let mut worker = match mem::take(&mut self.state) {
            State::Ready(worker) => worker,
            other => {
                self.state = other;
                return;
            },
        };
        let ticket = request.ticket();
        let language = match &request {
            Request::Stream { .. } => None,
            Request::Piece { audio, .. } | Request::Final { audio, .. } => {
                audio.language.as_deref()
            },
        }
        .unwrap_or(&config.language)
        .to_owned();
        let engine = config.engine;
        let wanted = match request {
            Request::Final {
                detail: Detail::Words,
                ..
            } => Wanted::Words(Cleanup {
                fillers: config.fillers(&language),
                corrections: config.vocabulary.clone(),
            }),
            Request::Stream { .. }
            | Request::Piece { .. }
            | Request::Final {
                detail: Detail::Text,
                ..
            } => Wanted::Text,
        };
        let terms = config.recognition_terms.clone();
        self.state = State::Transcribing {
            ticket,
            job: Job::spawn(|mut cancelled| async move {
                let heard = async {
                    match request {
                        Request::Stream { audio, .. } => {
                            let text = worker.transcribe_stream(audio, &language, &terms).await?;
                            Ok(text
                                .map(|text| Transcript::Text(Zeroizing::new(text)))
                                .map_or(Transcription::Abandoned, Transcription::Heard))
                        },
                        Request::Piece { audio: clip, .. } | Request::Final { audio: clip, .. } => {
                            transcript_of(&mut worker, clip, &language, engine, wanted, &terms)
                                .await
                                .map(Transcription::Heard)
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
        match &self.state {
            State::Loading(job) => job.cancel(),
            State::Transcribing { job, .. } => job.cancel(),
            State::Unavailable | State::Ready(_) => {},
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
        let (mut worker, answered) = self.into_worker().await?;
        if !*cancelled.borrow() && (answered || (probe && responds(&mut worker, cancelled).await)) {
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
        match self.state {
            State::Ready(worker) => Some((worker, false)),
            State::Loading(job) => job.task.await.ok()?.ok().map(|worker| (worker, false)),
            State::Transcribing { job, .. } => job
                .task
                .await
                .ok()?
                .ok()
                .map(|(worker, transcription)| (worker, transcription.answered())),
            State::Unavailable => None,
        }
    }
}

enum Transcription {
    Abandoned,
    Heard(Transcript<Native>),
    Cancelled,
}

impl Transcription {
    const fn answered(&self) -> bool {
        matches!(self, Self::Heard(_) | Self::Abandoned)
    }

    fn completion(self, ticket: Ticket) -> Completion<Native> {
        match self {
            Self::Heard(transcript) => Completion::Transcribed(ticket, Ok(transcript)),
            Self::Cancelled => Completion::Loaded(Ok(())),
            Self::Abandoned => Completion::Abandoned(ticket),
        }
    }
}

struct Job<T> {
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

enum Wanted {
    Text,
    Words(Cleanup),
}

struct Cleanup {
    fillers: Fillers,
    corrections: Vec<Correction>,
}

impl Cleanup {
    fn narration(&self, narration: Narration) -> Narration {
        Vocabulary::new(&self.corrections).narration(transcript::for_show(narration, self.fillers))
    }
}

fn invented_credits<'a>(
    engine: Engine,
    heard: impl Iterator<Item = &'a str> + Clone,
    audible: Duration,
) -> bool {
    engine == Engine::Whisper && credits::invented(heard, audible)
}

async fn transcript_of<W: Speech>(
    worker: &mut W,
    mut clip: Clip,
    language: &str,
    engine: Engine,
    wanted: Wanted,
    terms: &Terms,
) -> anyhow::Result<Transcript<Native>> {
    let wav = mem::take(&mut *clip.wav);
    Ok(match wanted {
        Wanted::Text => {
            let text = Zeroizing::new(worker.transcribe(wav, language, terms).await?);
            Transcript::Text(
                if invented_credits(engine, iter::once(text.as_str()), clip.audible) {
                    Zeroizing::default()
                } else {
                    text
                },
            )
        },
        Wanted::Words(cleanup) => {
            let narration = worker.transcribe_words(wav, language, terms).await?;
            let invented = match &narration {
                Narration::Text(text) => {
                    invented_credits(engine, iter::once(text.as_str()), clip.audible)
                },
                Narration::Words(words) => invented_credits(
                    engine,
                    words.iter().map(|word| word.text.as_str()),
                    clip.audible,
                ),
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
