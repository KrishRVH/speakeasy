use std::time::{Duration, Instant, SystemTime};

use async_channel::Sender;
use speakeasy_platform::{Delivery, InsertPermit, Inserted, Scope, Taps};
use speakeasy_show::{Ending, Journal, Narration, NarrationLoss, Recording};
use tokio::sync::watch;
use zeroize::Zeroize;

use crate::{
    config::Config,
    recognition_terms::Terms,
    show::{AudioLog, FrameLog, Kept, Publication, Reopened, SavedShow},
    sounds::Cue,
};

pub(crate) const STARTUP_CANCELLED: &str = "Local model startup cancelled";

pub(crate) const PUBLISH_STOPPED: &str =
    "Saving the Show stopped unexpectedly. Start the Show again.";

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

pub(crate) enum CaptureEvent {
    Streaming(SessionId, LiveAudio),
    Ready(SessionId),
    CheckpointLost(SessionId),
    Level(SessionId, f32),
    Piece(SessionId, u32, Sealed),
    Finished(SessionId, anyhow::Result<Option<Sealed>>),
}

impl CaptureEvent {
    pub(super) const fn session(&self) -> SessionId {
        match self {
            Self::Streaming(session, _)
            | Self::Ready(session)
            | Self::CheckpointLost(session)
            | Self::Level(session, _)
            | Self::Piece(session, _, _)
            | Self::Finished(session, _) => *session,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecognitionMode {
    Whole,
    Segmented,
    Streaming,
}

pub(crate) struct LiveAudio {
    pub rate: u32,
    pub frames: tokio::sync::mpsc::Receiver<zeroize::Zeroizing<Vec<u8>>>,
    pub tail: tokio::sync::oneshot::Receiver<zeroize::Zeroizing<Vec<u8>>>,
}

#[derive(Default)]
pub(crate) struct Sealed {
    pub wav: Vec<u8>,
    pub trimmed: Duration,
    pub partition: Option<Partition>,
}

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

pub(crate) enum ShowEvent {
    CheckpointLost(SessionId),
    MovieStopped(SessionId),
    Finished(SessionId, anyhow::Result<Box<Footage>>),
    Unchosen(SessionId, Unchosen),
}

impl ShowEvent {
    pub(super) const fn session(&self) -> SessionId {
        match self {
            Self::Finished(session, _)
            | Self::Unchosen(session, _)
            | Self::MovieStopped(session)
            | Self::CheckpointLost(session) => *session,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unchosen {
    Window,
    Area,
}

pub(crate) struct Footage {
    pub recording: Recording,
    pub journal: Journal,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Preservation {
    pub narration_lost: Option<NarrationLoss>,
    pub source_lost: bool,
    pub movie_lost: bool,
    pub audio_kept: bool,
}

pub(crate) enum Published {
    Recovered {
        show: SavedShow,
        preservation: Preservation,
        narrated_copy: bool,
    },
    Saved {
        show: SavedShow,
        ending: Ending,
        preservation: Preservation,
        copied: anyhow::Result<()>,
    },
    Cancelled,
}

pub(crate) trait Capturing: Send + 'static {
    fn finish(&self);

    fn retire(self) -> impl Future<Output = ()> + Send + 'static;
}

pub(crate) trait Listening: Capturing {
    fn whole(&self);

    fn expect_cue(&self);

    fn keep(&self, log: AudioLog, start: Duration, language: &str) -> bool;
}

pub(crate) trait Pinning: Capturing {
    fn pin(&self, taps: Taps);
}

pub(crate) trait Speech: Send + 'static {
    fn transcribe_stream(
        &mut self,
        audio: LiveAudio,
        language: &str,
        terms: &Terms,
    ) -> impl Future<Output = anyhow::Result<Option<String>>> + Send;

    fn transcribe(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> impl Future<Output = anyhow::Result<String>> + Send;

    fn transcribe_words(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> impl Future<Output = anyhow::Result<Narration>> + Send;

    fn probe_with_silence(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send;

    fn stop(&mut self) -> impl Future<Output = ()> + Send;
}

pub(crate) trait Ports: Send + 'static {
    type Microphone: Listening;
    type Screen: Pinning;
    type Speech: Speech;

    fn prepares_desktop(&self) -> bool;

    fn record_microphone(
        &self,
        id: SessionId,
        microphone: Option<&str>,
        recognition: RecognitionMode,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Self::Microphone>;

    fn keep(&self, recorded: SystemTime) -> anyhow::Result<Option<Kept>>;

    fn record_screen(
        &self,
        id: SessionId,
        started: Instant,
        scope: Scope,
        log: Option<FrameLog>,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<Self::Screen>;

    fn publish(
        &self,
        publication: Publication,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static;

    fn reopen(
        &self,
        interrupted: crate::show::Interrupted,
    ) -> impl Future<Output = anyhow::Result<Reopened>> + Send + 'static;

    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<Self::Speech>> + Send + 'static;

    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        delivery: Delivery,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static;

    fn cue(&self, cue: Cue);

    fn hold_cues(&self, held: bool);
}
