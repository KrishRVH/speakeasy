//! The side effects used by the session owner: recording devices, speech, insertion, and
//! publishing Shows. Tests replace them at this boundary; gesture, cancellation, and delivery
//! ordering stay real.

use std::{path::PathBuf, time::Instant};

use async_channel::Sender;
use speakeasy_platform::{Delivery, InsertPermit, Inserted, Inserter};
use speakeasy_show::{Action, Journal, Moment, Size, Stroke};
use tokio::sync::watch;

use crate::{
    audio::Capture,
    config::Config,
    local_speech::LocalSpeech,
    runtime::{CaptureEvent, SessionId, ShowEvent},
    show::{self, Canvas, Recorder},
};

/// The error a speech startup reports once cancelled.
pub(crate) const STARTUP_CANCELLED: &str = "Local model startup cancelled";

/// The failure a Show publication reports when its task ends without a result.
pub(crate) const PUBLISH_STOPPED: &str = "Publishing the Show stopped unexpectedly. Try again.";

pub(crate) trait Recording: Send + 'static {
    /// Requests the end of capture; the recording then reports its `Finished` event.
    fn finish(&self);

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

    /// Returns raw engine words timed from the start of `wav`, or untimed text when the engine
    /// gives no timings.
    fn transcribe_words(
        &self,
        wav: Vec<u8>,
        language: &str,
    ) -> impl Future<Output = anyhow::Result<speakeasy_show::Narration>> + Send;

    /// Transcribes silence and discards the text. Engines serialize requests, so completion proves
    /// that an abandoned inference has drained and the model still responds, which a health check
    /// cannot.
    fn probe_with_silence(&self) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Terminates the engine; resolves only once its process has been reaped.
    fn stop(&mut self) -> impl Future<Output = ()> + Send;
}

/// What a Show's screen side recorded.
pub(crate) struct Footage {
    /// The captured display's size in logical pixels.
    pub surface: Size,
    /// When recording stopped, on the Show's timeline.
    pub ended: Moment,
    pub journal: Journal,
    pub strokes: Vec<Stroke>,
    pub actions: Vec<Action>,
}

/// Everything a finished Show recorded, ready to choose its keyframes from.
pub(crate) struct Recorded {
    pub recording: speakeasy_show::Recording,
    pub journal: Journal,
}

/// What publishing a Show did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Published {
    /// The bundle was written here and its summary copied.
    Copied(PathBuf),
    /// Cancellation came first; nothing was written or copied.
    Cancelled,
}

pub(crate) trait Ports: Send + 'static {
    type Recording: Recording;
    type Show: Recording;
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

    /// Starts recording the display under the pointer for session `id`, whose timeline began at
    /// `started`, reporting to `events`.
    fn show(
        &self,
        id: SessionId,
        started: Instant,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<Self::Show>;

    /// Turns a finished Show into a bundle; once `permit` commits, writes it and copies its
    /// summary.
    fn publish(
        &self,
        recorded: Recorded,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static;

    /// Starts a speech worker; `cancelled` becoming true abandons startup.
    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<Self::Speech>> + Send + 'static;

    /// Submits `text` as `delivery` asks; no native input happens unless `permit` commits first.
    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        delivery: Delivery,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static;
}

pub(crate) struct Desktop {
    pub inserter: Inserter,
    /// Where a Show's live ink reaches the overlay.
    pub canvas: watch::Sender<Canvas>,
}

impl Ports for Desktop {
    type Recording = Capture;
    type Show = Recorder;
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

    fn show(
        &self,
        id: SessionId,
        started: Instant,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<Recorder> {
        Recorder::start(id, started, events, self.canvas.clone())
    }

    fn publish(
        &self,
        recorded: Recorded,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        show::publish(recorded, permit)
    }

    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<LocalSpeech>> + Send + 'static {
        LocalSpeech::start(config, cancelled)
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
