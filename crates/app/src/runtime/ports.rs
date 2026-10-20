//! The seam between the session owner and its side effects: recording devices, speech, insertion,
//! and publishing Shows, with the session identity and events that cross it. Tests replace the
//! side effects at this boundary; gesture, cancellation, and delivery ordering stay real.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use async_channel::Sender;
use speakeasy_platform::{Delivery, InsertPermit, Inserted};
use speakeasy_show::{Action, Journal, Moment, Size, Stroke};
use tokio::sync::watch;

use crate::config::Config;

/// The error a speech startup reports once cancelled.
pub(crate) const STARTUP_CANCELLED: &str = "Local model startup cancelled";

/// The failure a Show publication reports when its task ends without a result.
pub(crate) const PUBLISH_STOPPED: &str = "Publishing the Show stopped unexpectedly. Try again.";

/// Identifies one recording; capture events carry it, so late audio cannot reach a newer session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionId(u64);

impl SessionId {
    #[cfg(test)]
    pub(crate) const FIRST: Self = Self(1);

    pub(super) fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl From<SessionId> for u64 {
    fn from(id: SessionId) -> Self {
        id.0
    }
}

/// A capture thread's report about the session that opened it.
pub(crate) enum CaptureEvent {
    Ready(SessionId),
    Level(SessionId, f32),
    Finished(SessionId, anyhow::Result<Option<Sealed>>),
}

/// A Show recorder's report about the session that opened it.
pub(crate) enum ShowEvent {
    Finished(SessionId, anyhow::Result<Footage>),
}

impl CaptureEvent {
    pub(super) const fn session(&self) -> SessionId {
        match self {
            Self::Ready(session) | Self::Level(session, _) | Self::Finished(session, _) => *session,
        }
    }
}

/// Speech sealed for recognition: a WAV, and how much leading quiet was trimmed before it, which
/// places times the recognizer reports back on the recording's clock.
pub(crate) struct Sealed {
    pub wav: Vec<u8>,
    pub trimmed: Duration,
}

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
