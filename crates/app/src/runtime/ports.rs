use std::{
    ops::Range,
    time::{Duration, Instant, SystemTime},
};

use async_channel::Sender;
use speakeasy_core::machine::{Cue, RecognitionMode, SessionId};
use speakeasy_platform::{Delivery, Enter, InsertPermit, Inserted, Keep, Scope, Taps};
use speakeasy_show::{Ending, Framing, Journal, Narration, NarrationLoss, Recording, Take};
use tokio::sync::{mpsc, oneshot, watch};
use zeroize::Zeroizing;

use crate::{
    config::Config,
    recognition_terms::Terms,
    show::{AudioLog, FrameLog, Interrupted, Kept, Publication, Reopened, SavedShow},
};

pub(crate) const STARTUP_CANCELLED: &str = "Local model startup cancelled";

pub(crate) const PUBLISH_STOPPED: &str =
    "Saving the Show stopped unexpectedly. Start the Show again.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MicrophoneSource {
    Chosen,
    Default,
}

pub(crate) enum CaptureEvent {
    Streaming(SessionId, LiveAudio),
    Ready(SessionId, MicrophoneSource),
    CheckpointLost(SessionId),
    Level(SessionId, f32),
    Piece(SessionId, u32, Sealed),
    Finished(SessionId, anyhow::Result<Option<Sealed>>),
    Lost(SessionId, Sealed, Lost),
}

pub(crate) struct Lost {
    pub notice: &'static str,
    pub failure: anyhow::Error,
}

pub(crate) struct LiveAudio {
    pub rate: u32,
    pub frames: mpsc::Receiver<Zeroizing<Vec<u8>>>,
    pub tail: oneshot::Receiver<Zeroizing<Vec<u8>>>,
}

pub(crate) struct Sealed {
    pub wav: Zeroizing<Vec<u8>>,
    pub trimmed: Duration,
    pub audible: Duration,
    pub partition: Option<Partition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Partition {
    pub pieces: u32,
    pub tail: Range<usize>,
}

pub(crate) enum ShowEvent {
    CheckpointLost(SessionId),
    MovieStopped(SessionId),
    Finished(SessionId, anyhow::Result<Box<Footage>>),
    Unchosen(SessionId, Unchosen),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unchosen {
    Window,
    Area,
}

pub(crate) struct Footage {
    pub recording: Recording,
    pub journal: Journal,
    pub framing: Framing,
}

impl From<Take> for Footage {
    fn from(take: Take) -> Self {
        let framing = take.framing();
        let (recording, journal) = take.finish();
        Self {
            recording,
            journal,
            framing,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Preservation {
    pub narration_lost: Option<NarrationLoss>,
    pub source_lost: bool,
    pub movie_lost: bool,
    pub audio: Audio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Audio {
    #[default]
    Settled,
    KeptForRetry,
    VoiceCutShort,
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
    type Awake: Send + 'static;

    fn prepares_desktop(&self) -> bool;

    fn screen_unstopped(&self) -> bool;

    fn keep_awake(&self, keep: Keep) -> Option<Self::Awake>;

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
        interrupted: Interrupted,
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
        enter: Enter,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static;

    fn cue(&self, cue: Cue);

    fn hold_cues(&self, held: bool);
}
