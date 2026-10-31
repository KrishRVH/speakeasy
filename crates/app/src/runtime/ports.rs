//! The seam between the session owner and its side effects: recording devices, speech, insertion,
//! publishing Shows, and sound cues, with the session identity and events that cross it. Tests
//! replace the side effects at this boundary; gesture, cancellation, and delivery ordering stay
//! real.

use std::time::{Duration, Instant, SystemTime};

use async_channel::Sender;
use speakeasy_platform::{Delivery, InsertPermit, Inserted, Scope, Taps};
use speakeasy_show::{Ending, Journal, Narration, NarrationLoss, Recording};
use tokio::sync::watch;
use zeroize::Zeroize;

use crate::{
    config::Config,
    show::{AudioLog, FrameLog, Kept, Publication, Reopened, SavedShow},
    sounds::Cue,
};

/// The error a speech startup reports once cancelled.
pub(crate) const STARTUP_CANCELLED: &str = "Local model startup cancelled";

/// The failure a Show publication reports when its task ends without a result.
pub(crate) const PUBLISH_STOPPED: &str =
    "Saving the Show stopped unexpectedly. Start the Show again.";

/// Identifies one recording; capture events carry it, so late audio cannot reach a newer session.
/// An owner numbers its sessions in order, so a later one compares greater.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
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
    /// The Show audio checkpoint stopped; live transcription remains available.
    CheckpointLost(SessionId),
    /// The latest microphone level, for the pill's meter.
    Level(SessionId, f32),
    /// An abutting piece of ordinary dictation, numbered from zero. The final WAV still keeps all
    /// audio, so a missing or failed piece can fall back to whole-recording recognition.
    Piece(SessionId, u32, Sealed),
    /// Capture ended; `None` when the recording was too short or held too little speech to seal.
    Finished(SessionId, anyhow::Result<Option<Sealed>>),
}

impl CaptureEvent {
    pub(super) const fn session(&self) -> SessionId {
        match self {
            Self::Ready(session)
            | Self::CheckpointLost(session)
            | Self::Level(session, _)
            | Self::Piece(session, _, _)
            | Self::Finished(session, _) => *session,
        }
    }
}

/// Speech prepared for recognition. Leading quiet trimmed from the whole recording places the
/// recognizer's times on its clock. Pieces move into the worker; the whole recording stays available
/// for fallback. Dropping this value zeroes any WAV it still owns.
#[derive(Default)]
pub(crate) struct Sealed {
    pub wav: Vec<u8>,
    pub trimmed: Duration,
    pub partition: Option<Partition>,
}

/// Pieces already sent by capture, followed by the tail within the final WAV's PCM payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Partition {
    pub pieces: u32,
    pub tail: std::ops::Range<usize>,
}

impl Drop for Sealed {
    fn drop(&mut self) {
        self.wav.zeroize();
    }
}

/// A Show recorder's report about the session that opened it.
pub(crate) enum ShowEvent {
    /// Checkpointing stopped; the live recording still proceeds in memory.
    CheckpointLost(SessionId),
    /// Recording ended with its footage, boxed since it is far larger than the other reports, or
    /// failed.
    Finished(SessionId, anyhow::Result<Box<Footage>>),
    /// The Show's scope found nothing to record, so it recorded nothing.
    Unchosen(SessionId, Unchosen),
}

impl ShowEvent {
    pub(super) const fn session(&self) -> SessionId {
        match self {
            Self::Finished(session, _)
            | Self::Unchosen(session, _)
            | Self::CheckpointLost(session) => *session,
        }
    }
}

/// What a Show's scope found nothing to record in; a Show never records more than it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unchosen {
    /// W found no window it can record under the pointer.
    Window,
    /// A finished, was clicked away, or could not shade its display before an area was dragged
    /// out.
    Area,
}

/// What a Show's screen side recorded, up to the moment recording stopped, and whether that came
/// before the Show was finished; publishing adds the narration its recording still lacks.
pub(crate) struct Footage {
    pub recording: Recording,
    pub journal: Journal,
}

/// What publishing a Show did. A publication that fails before its bundle persists reports an
/// error instead; a checkpoint can remain available for recovery.
pub(crate) enum Published {
    /// An interrupted Show was recovered, without copying or inventing narration.
    Recovered {
        show: SavedShow,
        source_lost: bool,
        lost: Option<NarrationLoss>,
        audio_kept: bool,
        narrated_copy: bool,
    },
    /// The bundle persisted as `show`, with how its screen recording ended and why its narration
    /// was lost, if it was; `copied` is whether its summary then reached the clipboard.
    Saved {
        show: SavedShow,
        ending: Ending,
        lost: Option<NarrationLoss>,
        source_lost: bool,
        audio_kept: bool,
        copied: anyhow::Result<()>,
    },
    /// Cancellation came first; no bundle appeared and nothing was copied.
    Cancelled,
}

/// A recording device the owner opens, finishes, and retires: the microphone or a Show's
/// screen recorder.
pub(crate) trait Capturing: Send + 'static {
    /// Requests the end of capture; the recording then reports how it ended: `Finished`, or
    /// `Unchosen` for a Show whose scope found nothing to record.
    fn finish(&self);

    /// Cancels capture; resolves only once the device has been released.
    fn retire(self) -> impl Future<Output = ()> + Send + 'static;
}

/// The microphone, which can hear Speakeasy's own cues.
pub(crate) trait Listening: Capturing {
    /// Disables further pieces after Show promotion; its full audio stays available for timing.
    fn whole(&self);
    /// A cue is about to sound: until it has passed, only a voice beneath it counts as speech, so
    /// a recording of nothing but the cue stays silent.
    fn expect_cue(&self);

    /// Attaches the sole Show audio producer. False means capture already sealed or stopped.
    fn keep(&self, log: AudioLog, start: Duration, language: &str) -> bool;
}

/// A Show's screen recorder, which takes numbered pins.
pub(crate) trait Pinning: Capturing {
    /// Asks for a numbered pin for `taps`; the Show's tracker decides whether one drops.
    fn pin(&self, taps: Taps);
}

pub(crate) trait Speech: Send + 'static {
    /// Returns raw engine text. The worker applies transcript cleanup before insertion.
    fn transcribe(
        &mut self,
        wav: Vec<u8>,
        language: &str,
    ) -> impl Future<Output = anyhow::Result<String>> + Send;

    /// Returns raw engine words timed from the start of `wav`, or untimed text when the engine
    /// gives no timings or any time is missing, negative, out of range, or reversed; times are
    /// never guessed.
    fn transcribe_words(
        &mut self,
        wav: Vec<u8>,
        language: &str,
    ) -> impl Future<Output = anyhow::Result<Narration>> + Send;

    /// Transcribes silence and discards the text. Engines serialize requests, so completion proves
    /// that an abandoned inference has drained and the model still responds, which a health check
    /// cannot.
    fn probe_with_silence(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send;

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
        segmenting: bool,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Self::Microphone>;

    /// Starts private checkpointing after explicit Show promotion. Simulated ports return None.
    fn keep(&self, recorded: SystemTime) -> anyhow::Result<Option<Kept>>;

    /// Starts recording what `scope` names, near the pointer, for session `id`, whose timeline
    /// began at `started`, reporting to `events`.
    fn record_screen(
        &self,
        id: SessionId,
        started: Instant,
        scope: Scope,
        log: Option<FrameLog>,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<Self::Screen>;

    /// Publishes a finished Show under the session's authority; recovery never copies a summary.
    fn publish(
        &self,
        publication: Publication,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static;

    /// Reopens a checkpoint off the owner thread, including audio prepared for its speech worker.
    fn reopen(
        &self,
        interrupted: crate::show::Interrupted,
    ) -> impl Future<Output = anyhow::Result<Reopened>> + Send + 'static;

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
