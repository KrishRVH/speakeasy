//! One warm worker, or one job that owns it. Replacement moves the previous
//! job out of the observation path, then awaits its cleanup before loading.
//! Errors reap the owned worker; unwinding remains an emergency kill-on-drop.
use super::{Config, ModelState, Ports, Speech};
use std::time::Duration;
use tokio::{sync::watch, task::JoinHandle};

type Joined<T> = Result<anyhow::Result<T>, tokio::task::JoinError>;
pub(super) enum Completion<W> {
    Loaded(Joined<W>),
    Transcribed(Joined<(W, Option<String>)>),
}
pub(super) struct Job<T> {
    cancel: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<T>>,
}
pub(super) enum Worker<W> {
    Unavailable,
    Loading(Job<W>),
    Ready(W),
    Transcribing(Job<(W, Option<String>)>),
    Recovering(Job<W>),
}
impl<W: Speech> Worker<W> {
    pub(super) fn model(&self) -> ModelState {
        match self {
            Self::Unavailable => ModelState::Unavailable,
            Self::Loading(_) | Self::Recovering(_) => ModelState::Loading,
            Self::Ready(_) | Self::Transcribing(_) => ModelState::Ready,
        }
    }
    pub(super) fn is_transcribing(&self) -> bool {
        matches!(self, Self::Transcribing(_))
    }
    pub(super) fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }
    pub(super) async fn completed(&mut self) -> Completion<W> {
        match self {
            Self::Loading(job) | Self::Recovering(job) => Completion::Loaded((&mut job.task).await),
            Self::Transcribing(job) => Completion::Transcribed((&mut job.task).await),
            Self::Unavailable | Self::Ready(_) => std::future::pending().await,
        }
    }
    // Consume a cancelled/replaced lane: its completion can never reach Owner.
    async fn into_worker(self) -> Option<(W, bool)> {
        match self {
            Self::Ready(worker) => Some((worker, false)),
            Self::Loading(job) | Self::Recovering(job) => {
                job.task.await.ok()?.ok().map(|worker| (worker, false))
            },
            Self::Transcribing(job) => job
                .task
                .await
                .ok()?
                .ok()
                .map(|(worker, text)| (worker, text.is_some())),
            Self::Unavailable => None,
        }
    }
    pub(super) fn replace<P: Ports<Speech = W>>(
        &mut self,
        ports: &P,
        config: &Config,
        reuse: bool,
    ) {
        let previous = std::mem::replace(self, Self::Unavailable);
        previous.request_stop();
        let (cancel, mut cancelled) = watch::channel(false);
        let load = ports.load(config.clone(), cancelled.clone());
        let gpu = config.use_gpu;
        let task = tokio::spawn(async move {
            if let Some((mut worker, completed)) = previous.into_worker().await {
                let healthy = reuse
                    && !*cancelled.borrow()
                    && (completed
                        || (gpu
                            && tokio::select! {
                                biased;
                                _ = cancelled.changed() => false,
                                result = tokio::time::timeout(Duration::from_secs(2), worker.idle()) => matches!(result, Ok(Ok(()))),
                            }));
                if healthy && !*cancelled.borrow() {
                    return Ok(worker);
                }
                worker.stop().await;
            }
            anyhow::ensure!(!*cancelled.borrow(), "Local model startup cancelled");
            load.await
        });
        let job = Job { cancel, task };
        *self = if reuse {
            Self::Recovering(job)
        } else {
            Self::Loading(job)
        };
    }
    pub(super) fn transcribe(&mut self, wav: Vec<u8>, language: String) -> Result<(), Vec<u8>> {
        let previous = std::mem::replace(self, Self::Unavailable);
        let Self::Ready(mut worker) = previous else {
            *self = previous;
            return Err(wav);
        };
        let (cancel, mut cancelled) = watch::channel(false);
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = cancelled.changed() => Ok(None),
                result = worker.transcribe(wav, &language) => result.map(Some),
            };
            match result {
                Ok(text) => Ok((worker, text)),
                Err(error) => {
                    worker.stop().await;
                    Err(error)
                },
            }
        });
        *self = Self::Transcribing(Job { cancel, task });
        Ok(())
    }
    pub(super) fn request_stop(&self) {
        match self {
            Self::Loading(job) | Self::Recovering(job) => {
                job.cancel.send_replace(true);
            },
            Self::Transcribing(job) => {
                job.cancel.send_replace(true);
            },
            Self::Unavailable | Self::Ready(_) => {},
        }
    }
    pub(super) async fn stop(self) {
        self.request_stop();
        if let Some((mut worker, _)) = self.into_worker().await {
            worker.stop().await;
        }
    }
}
