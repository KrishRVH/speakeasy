//! One app-owned preview read drains before its replacement; the view owns only its cancellable receipt.

mod frame;

use std::{convert::Infallible, path::PathBuf, pin::Pin, sync::Arc, time::Duration};

use async_channel::Sender;
use gpui::RenderImage;
use speakeasy_platform::OwnedThread;
use speakeasy_show::Moment;

#[derive(Clone, Debug)]
pub(crate) enum Content {
    Png(String),
    Source {
        index: Arc<speakeasy_show::Source>,
        at: Moment,
    },
    Video {
        span: std::ops::Range<Duration>,
        annotations: Moment,
        scene: Option<Arc<speakeasy_show::Annotations>>,
    },
}

pub(crate) struct Request {
    pub directory: PathBuf,
    pub content: Content,
    pub at: Moment,
    pub annotated: bool,
    pub fixture: Option<Arc<tempfile::TempDir>>,
}

pub(crate) struct Decoded {
    pub image: Arc<RenderImage>,
    pub at: Duration,
    pub annotations: super::store::movie::AnnotationState,
}

pub(crate) type Receipt = Pin<Box<dyn Future<Output = Result<Decoded, String>> + Send>>;
pub(crate) type Wait = Pin<Box<dyn Future<Output = ()> + Send>>;

pub(crate) enum Work {
    Reading(Receipt),
    Waiting(Wait),
}

#[derive(Default)]
pub(crate) struct Reads {
    pending: Option<Pending>,
}

struct Pending {
    stop: Sender<Infallible>,
    thread: OwnedThread,
}

impl Reads {
    pub(crate) fn submit(&mut self, request: Request) -> anyhow::Result<Work> {
        self.start(move |cancelled| frame::read(request, cancelled))
    }

    fn start(
        &mut self,
        read: impl FnOnce(&dyn Fn() -> bool) -> Result<Decoded, String> + Send + 'static,
    ) -> anyhow::Result<Work> {
        if let Some(pending) = &self.pending
            && !pending.thread.is_finished()
        {
            return Ok(Work::Waiting(Box::pin(pending.thread.exited())));
        }
        let (stop, stopping) = async_channel::bounded::<Infallible>(1);
        let (done, result) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("show-preview", move || {
            let cancelled = || stopping.is_closed() || done.is_closed();
            let image = read(&cancelled);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Replacing or closing the view cancels its receipt; the owner retains this thread through native cleanup"
            )]
            let _ = done.try_send(image);
        })?;
        let exited = thread.exited();
        self.pending = Some(Pending { stop, thread });
        Ok(Work::Reading(Box::pin(async move {
            exited.await;
            result.recv().await.unwrap_or_else(|_| {
                Err("Preview stopped unexpectedly. Select the frame again.".into())
            })
        })))
    }

    pub(crate) fn cancel(&self) {
        if let Some(pending) = &self.pending {
            pending.stop.close();
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

impl Drop for Reads {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests;
