use std::{convert::Infallible, path::PathBuf, pin::Pin};

use async_channel::{Receiver, Sender};
use serde::Serialize;
use speakeasy_platform::OwnedThread;

mod payload;
mod service;

const CANCELLED: &str = "OpenCode handoff cancelled before submission.";
const STOPPED: &str = "OpenCode stopped unexpectedly. Check the session before sending again.";

pub(crate) struct Request {
    pub directory: PathBuf,
    pub session: String,
    pub expected_endpoint: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Session {
    pub id: String,
    pub title: Option<String>,
    pub directory: String,
    pub updated: u64,
}

#[derive(Serialize)]
pub(crate) struct Listing {
    pub endpoint: String,
    pub sessions: Vec<Session>,
    pub truncated: bool,
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub(crate) enum Outcome {
    Queued {
        session: String,
        message: String,
        attachments: usize,
        bytes: usize,
    },
    NotSubmitted {
        reason: String,
    },
    Unknown {
        reason: String,
    },
}

impl Outcome {
    pub(crate) fn note(&self) -> String {
        match self {
            Self::Queued {
                session,
                attachments,
                ..
            } => format!(
                "Queued to OpenCode session {session} with {attachments} contact sheets. The recipient's copy does not expire with this Show."
            ),
            Self::NotSubmitted { reason } | Self::Unknown { reason } => reason.clone(),
        }
    }

    fn refused(reason: &str) -> Self {
        Self::NotSubmitted {
            reason: reason.into(),
        }
    }

    fn unknown() -> Self {
        Self::Unknown {
            reason:
                "OpenCode admission could not be confirmed. Check the session before sending again."
                    .into(),
        }
    }
}

pub(crate) type SessionReceipt = Receipt<Result<Listing, String>>;

pub(crate) type Receipt<T = Outcome> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Default)]
pub(crate) struct Deliveries {
    pending: Option<Pending>,
}

struct Pending {
    stopping: Sender<Infallible>,
    thread: OwnedThread,
}

struct Cancellation {
    stopping: Receiver<Infallible>,
    interested: Receiver<Infallible>,
}

impl Cancellation {
    fn cancelled(&self) -> bool {
        self.stopping.is_closed() || self.interested.is_closed()
    }

    async fn wait(&self) {
        tokio::select! {
            _ = self.stopping.recv() => {},
            _ = self.interested.recv() => {},
        }
    }
}

impl Deliveries {
    pub(crate) fn is_busy(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| !pending.thread.is_finished())
    }

    pub(crate) fn list(&mut self) -> anyhow::Result<Receipt<Result<Listing, String>>> {
        self.list_from(service::registration_path())
    }

    fn list_from(
        &mut self,
        registration: Result<PathBuf, &'static str>,
    ) -> anyhow::Result<Receipt<Result<Listing, String>>> {
        self.start(
            move |cancel| {
                let operation = async {
                    let registration = registration?;
                    service::list(&registration, &cancel).await
                };
                run(operation).flatten().map_err(str::to_owned)
            },
            Err("OpenCode session lookup stopped unexpectedly. Try again.".into()),
        )
    }

    pub(crate) fn send(&mut self, request: Request) -> anyhow::Result<Receipt> {
        self.send_from(request, service::registration_path())
    }

    fn send_from(
        &mut self,
        request: Request,
        registration: Result<PathBuf, &'static str>,
    ) -> anyhow::Result<Receipt> {
        self.start(
            move |cancel| {
                let operation = async {
                    match registration {
                        Ok(registration) => service::send(request, &registration, &cancel).await,
                        Err(reason) => Outcome::refused(reason),
                    }
                };
                run(operation).unwrap_or_else(Outcome::refused)
            },
            Outcome::Unknown {
                reason: STOPPED.into(),
            },
        )
    }

    fn start<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(Cancellation) -> T + Send + 'static,
        stopped: T,
    ) -> anyhow::Result<Receipt<T>> {
        anyhow::ensure!(
            !self.is_busy(),
            "An OpenCode operation is still finishing. Try again shortly."
        );
        let (stopping, cancelled) = async_channel::bounded(1);
        let (interest, interested) = async_channel::bounded(1);
        let (done, receipt) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("show-opencode", move || {
            let result = work(Cancellation {
                stopping: cancelled,
                interested,
            });
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A closed view drops its receipt; an admitted POST still remains owned until completion"
            )]
            let _ = done.try_send(result);
        })?;
        let exited = thread.exited();
        self.pending = Some(Pending { stopping, thread });
        Ok(Box::pin(async move {
            let _interest = interest;
            exited.await;
            receipt.recv().await.unwrap_or(stopped)
        }))
    }

    pub(crate) fn cancel(&self) {
        if let Some(pending) = &self.pending {
            pending.stopping.close();
        }
    }

    pub(crate) fn drained(&self) -> impl Future<Output = ()> + use<> {
        let pending = self.pending.as_ref().map(|pending| pending.thread.exited());
        async move {
            if let Some(pending) = pending {
                pending.await;
            }
        }
    }
}

impl Drop for Deliveries {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn run<T>(operation: impl Future<Output = T>) -> Result<T, &'static str> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_error| "Cannot start the OpenCode operation. Try again.")?;
    Ok(runtime.block_on(operation))
}

#[cfg(test)]
mod tests;
