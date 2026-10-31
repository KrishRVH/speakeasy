//! Explicit Show copies own their preparation and native write until cleanup acknowledges. A
//! dropped receipt, lifecycle change, or newer recording cancels before clipboard replacement.

use std::{convert::Infallible, path::PathBuf, pin::Pin, sync::Arc};

use anyhow::Context as _;
use async_channel::{Receiver, Sender};
use gpui::RenderImage;
use speakeasy_platform::{ClipboardContent, IdleInput, OwnedThread};

use super::store::recall;

const CANCELLED: &str =
    "Copy cancelled because the request ended or dictation changed. Try again when idle.";

pub(crate) enum Request {
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    Saved(super::SavedShow),
    Summary(PathBuf),
    Moment {
        text: String,
        image: Arc<RenderImage>,
    },
}

impl Request {
    fn prepare(self) -> anyhow::Result<Option<ClipboardContent>> {
        match self {
            #[cfg(any(target_os = "windows", target_os = "macos", test))]
            Self::Saved(show) => show.summary()?.map(ClipboardContent::text).transpose(),
            Self::Summary(directory) => ClipboardContent::text(
                recall::summary(&directory).context("Cannot read summary.md")?,
            )
            .map(Some),
            Self::Moment { text, image } => {
                let pixels = image
                    .as_bytes(0)
                    .context("The selected image is unavailable")?;
                let size = image.size(0);
                ClipboardContent::moment(
                    text,
                    size.width.into(),
                    size.height.into(),
                    pixels.to_vec(),
                )
                .map(Some)
            },
        }
    }
}

pub(crate) enum Outcome {
    Copied,
    Missing,
}

pub(crate) type Receipt = Pin<Box<dyn Future<Output = Result<Outcome, String>> + Send>>;

#[derive(Default)]
pub(crate) struct Copies {
    pending: Option<Pending>,
}

struct Pending {
    stopping: Sender<Infallible>,
    thread: OwnedThread,
}

impl Copies {
    pub(crate) fn is_busy(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| !pending.thread.is_finished())
    }

    /// `input` observes a running owner's idle generation. With input stopped, the shell cancels
    /// this copy before starting an owner. The receiver's lifetime is the requesting view's intent.
    pub(crate) fn submit(
        &mut self,
        request: Request,
        input: Option<IdleInput>,
    ) -> anyhow::Result<Receipt> {
        self.start(request, input, |content, authorized| {
            speakeasy_platform::copy_content(content, authorized)
        })
    }

    fn start(
        &mut self,
        request: Request,
        input: Option<IdleInput>,
        write: impl FnOnce(&ClipboardContent, &dyn Fn() -> bool) -> anyhow::Result<bool>
        + Send
        + 'static,
    ) -> anyhow::Result<Receipt> {
        anyhow::ensure!(
            !self.is_busy(),
            "A clipboard copy is still finishing. Try again shortly."
        );
        let (stopping, stopped) = async_channel::bounded::<Infallible>(1);
        let (done, receipt) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("show-copy", move || {
            let authorized = || {
                !stopped.is_closed()
                    && !done.is_closed()
                    && input.as_ref().is_none_or(IdleInput::is_current)
            };
            let attempt = || {
                anyhow::ensure!(authorized(), CANCELLED);
                let content = request.prepare()?;
                anyhow::ensure!(authorized(), CANCELLED);
                let Some(content) = content else {
                    return Ok(Outcome::Missing);
                };
                anyhow::ensure!(write(&content, &authorized)?, CANCELLED);
                Ok(Outcome::Copied)
            };
            let result = attempt()
                .map_err(|error: anyhow::Error| format!("Cannot copy this Show: {error:#}"));
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Closing the requesting view cancels uncommitted work and drops its receipt; committed native work still drains"
            )]
            let _ = done.try_send(result);
        })?;
        let completion = Box::pin(completed(receipt, thread.exited()));
        self.pending = Some(Pending { stopping, thread });
        Ok(completion)
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

async fn completed(
    result: Receiver<Result<Outcome, String>>,
    exited: impl Future<Output = ()>,
) -> Result<Outcome, String> {
    // The native result can arrive just before its thread exits. Availability must not be
    // announced until the owner may safely accept the next request.
    exited.await;
    result
        .recv()
        .await
        .unwrap_or_else(|_| Err("Copy stopped unexpectedly. Try again when idle.".into()))
}

impl Drop for Copies {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests;
