//! The seam between the session owner and its side effects: recording devices, speech, insertion,
//! publishing Shows, and sound cues, with the session identity and events that cross it. Tests
//! replace the side effects at this boundary; gesture, cancellation, and delivery ordering stay
//! real.

use std::time::{Duration, Instant};

use async_channel::Sender;
use speakeasy_platform::{Delivery, InsertPermit, Inserted, Scope, Taps};
use speakeasy_show::{Journal, Narration, Recording};
use tokio::sync::watch;
use zeroize::Zeroize;

use crate::{config::Config, sounds::Cue};

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
    /// The first samples arrived; a capture reports it before any audio it seals.
    Ready(SessionId),
    /// The latest microphone level, for the pill's meter.
    Level(SessionId, f32),
    /// Capture ended; `None` when the recording was too short or held too little speech to seal.
    Finished(SessionId, anyhow::Result<Option<Sealed>>),
}

impl CaptureEvent {
    pub(super) const fn session(&self) -> SessionId {
        match self {
            Self::Ready(session) | Self::Level(session, _) | Self::Finished(session, _) => *session,
        }
    }
}

/// Speech sealed for transcription: a WAV, and how much leading quiet was trimmed before it, which
/// places times the recognizer reports back on the recording's clock. The WAV is zeroed on drop
/// wherever the audio is discarded; transcription takes it with `mem::take` first.
pub(crate) struct Sealed {
    pub wav: Vec<u8>,
    pub trimmed: Duration,
}

impl Drop for Sealed {
    fn drop(&mut self) {
        self.wav.zeroize();
    }
}

/// A Show recorder's report about the session that opened it.
pub(crate) enum ShowEvent {
    Finished(SessionId, anyhow::Result<Footage>),
}

/// What a Show's screen side recorded, up to the moment recording stopped; publishing adds the
/// narration its recording still lacks.
pub(crate) struct Footage {
    pub recording: Recording,
    pub journal: Journal,
}

/// What publishing a Show did.
pub(crate) enum Published {
    /// The bundle was written and its summary copied.
    Copied,
    /// Cancellation came first; no bundle appeared and nothing was copied.
    Cancelled,
}

/// A recording device the owner opens, finishes, and retires: the microphone or a Show's
/// screen recorder.
pub(crate) trait Capturing: Send + 'static {
    /// Requests the end of capture; the recording then reports its `Finished` event.
    fn finish(&self);

    /// Cancels capture; resolves only once the device has been released.
    fn retire(self) -> impl Future<Output = ()> + Send + 'static;
}

/// The microphone, which can hear Speakeasy's own cues.
pub(crate) trait Listening: Capturing {
    /// A cue is about to sound: until it has passed, only a voice beneath it counts as speech, so
    /// a recording of nothing but the cue stays silent.
    fn expect_cue(&self);
}

/// A Show's screen recorder, which takes numbered pins.
pub(crate) trait Pinning: Capturing {
    /// Asks for a numbered pin for `taps`; the Show's tracker decides whether one drops.
    fn pin(&self, taps: Taps);
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
    ) -> impl Future<Output = anyhow::Result<Narration>> + Send;

    /// Transcribes silence and discards the text. Engines serialize requests, so completion proves
    /// that an abandoned inference has drained and the model still responds, which a health check
    /// cannot.
    fn probe_with_silence(&self) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Terminates the engine; resolves only once its process has been reaped.
    fn stop(&mut self) -> impl Future<Output = ()> + Send;
}

pub(crate) trait Ports: Send + 'static {
    type Microphone: Listening;
    type Screen: Pinning;
    type Speech: Speech;

    /// Whether input must await `DesktopReady` before it can start capture.
    fn prepares_desktop(&self) -> bool;

    /// Opens `microphone`, or the system default when it is `None`, reporting to `events` as `id`.
    fn record_microphone(
        &self,
        id: SessionId,
        microphone: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Self::Microphone>;

    /// Starts recording what `scope` names, near the pointer, for session `id`, whose timeline
    /// began at `started`, reporting to `events`.
    fn record_screen(
        &self,
        id: SessionId,
        started: Instant,
        scope: Scope,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<Self::Screen>;

    /// Turns a finished Show's footage and its narration, already on the Show's timeline, into
    /// a bundle, which appears with its summary copied only once `permit` commits.
    fn publish(
        &self,
        footage: Footage,
        narration: Narration,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static;

    /// Starts a speech worker; `cancelled` becoming true abandons startup. Nothing starts until the
    /// future is first polled, because a replacement is built before its predecessor has exited.
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

    /// Plays `cue` now, or not at all.
    fn cue(&self, cue: Cue);

    /// Keeps the cue output open while `held`, so no cue waits for a device to open; once
    /// released, it closes shortly after the last cue.
    fn hold_cues(&self, held: bool);
}
