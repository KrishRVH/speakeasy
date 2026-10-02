//! The three side effects used by the session owner. Tests replace devices and speech at this
//! boundary; gesture, cancellation, and insertion ordering stay real.

use async_channel::Sender;
#[cfg(target_os = "macos")]
use speakeasy_platform::Inserter;
use speakeasy_platform::{Delivery, InsertPermit, Inserted};
use tokio::sync::watch;

#[cfg(target_os = "macos")]
use crate::{audio::Capture, local_speech::LocalSpeech};
use crate::{
    config::Config,
    runtime::{CaptureEvent, SessionId},
};

/// The error a speech startup reports once cancelled.
pub(crate) const STARTUP_CANCELLED: &str = "Local model startup cancelled";

pub(crate) trait Recording: Send + 'static {
    /// Requests the end of capture; the recording then reports `CaptureEvent::Finished`.
    fn finish(&self);

    /// Asks the recording to report `CaptureEvent::Paused` with its audio at each pause in speech.
    fn speculate(&self);

    /// Cancels capture; resolves only once the device has been released.
    fn retire(self) -> impl Future<Output = ()> + Send + 'static;
}

pub(crate) trait Speech: Send + Sync + 'static {
    /// Returns raw engine text. The worker applies transcript cleanup before insertion.
    fn transcribe(
        &self,
        wav: Vec<u8>,
        language: &str,
    ) -> impl Future<Output = anyhow::Result<String>> + Send;

    /// Transcribes silence and discards the text. Engines serialize requests, so completion proves
    /// that an abandoned inference has drained and the model still responds, which a health check
    /// cannot.
    fn probe_with_silence(&self) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Terminates the engine; resolves only once its process has been reaped.
    fn stop(&mut self) -> impl Future<Output = ()> + Send;
}

pub(crate) trait Ports: Send + 'static {
    type Recording: Recording;
    type Speech: Speech;

    /// Whether input must await `DesktopReady` before it can start capture.
    fn prepares_desktop(&self) -> bool;

    /// Opens `microphone`, or the system default when it is `None`, reporting to `events` as `id`.
    fn record(
        &self,
        id: SessionId,
        microphone: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Self::Recording>;

    /// Starts a speech worker; `cancelled` becoming true abandons startup.
    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<Self::Speech>> + Send + 'static;

    /// Hands an ended session's stage timings, numbers only, to whoever asked for them.
    fn report_timing(&self, _line: String) {}

    /// Submits `text` as `delivery` asks; no native input happens unless `permit` commits first.
    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        delivery: Delivery,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static;
}

/// The native microphone, local engine, and insertion adapter.
#[cfg(target_os = "macos")]
pub(crate) struct Desktop {
    pub inserter: Inserter,
    /// Whether `SPEAKEASY_TIMING` asked for each session's stage timings on standard error.
    pub timing: bool,
}

#[cfg(target_os = "macos")]
impl Ports for Desktop {
    type Recording = Capture;
    type Speech = LocalSpeech;

    fn prepares_desktop(&self) -> bool {
        true
    }

    fn record(
        &self,
        id: SessionId,
        microphone: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Capture> {
        Capture::start(id, microphone, events)
    }

    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<LocalSpeech>> + Send + 'static {
        LocalSpeech::start(config, cancelled)
    }

    fn report_timing(&self, line: String) {
        if self.timing {
            eprintln!("{line}");
        }
    }

    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        delivery: Delivery,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let inserter = self.inserter.clone();
        async move { inserter.insert(text, permit, delivery).await }
    }
}
