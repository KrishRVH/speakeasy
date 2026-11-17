use std::{
    mem,
    ops::Range,
    time::{Duration, Instant},
};

use speakeasy_core::machine::{self, Payload, RecognitionMode, Settings};
use speakeasy_platform::{InsertPermit, Scope, Taps};
use speakeasy_show::{Ending, Narration, NarrationLoss};
use tokio::{sync::mpsc, task::JoinError};
use zeroize::Zeroizing;

use super::{
    PracticeEvent,
    ports::{Footage, LiveAudio, Partition, Preservation, Published, Sealed, Unchosen},
};
use crate::{
    config::{Change, Config, Engine},
    control::Call,
    show::{AudioLog, Checkpointing, FrameLog, Interrupted, Reopened, SavedShow},
    transcript::style::Style,
};

pub(super) struct Native;

impl Payload for Native {
    type Audio = Clip;
    type AudioLog = AudioLog;
    type Call = Call;
    type Checkpoint = Checkpointing;
    type Config = Config;
    type Footage = Box<Footage>;
    type FrameLog = FrameLog;
    type Heard = Zeroizing<String>;
    type Instant = Instant;
    type Interrupted = Interrupted;
    type Level = f32;
    type Live = LiveAudio;
    type Narration = Narration;
    type Pending = bool;
    type Permit = InsertPermit;
    type Practice = mpsc::Sender<PracticeEvent>;
    type Range = Range<usize>;
    type Receipt = async_channel::Sender<Result<SavedShow, String>>;
    type Recovered = Restored;
    type Saved = Copied;
    type Scope = Scope;
    type Shown = SavedShow;
    type Span = Duration;
    type Style = Style;
    type Taps = Taps;
    type Text = String;
    type Unchosen = Unchosen;

    fn settings(config: &Config) -> Settings {
        Settings {
            play_sounds: config.play_sounds,
            recognition: if config.engine == Engine::Whisper {
                RecognitionMode::Whole
            } else if config.uses_nemotron() {
                RecognitionMode::Streaming
            } else {
                RecognitionMode::Segmented
            },
            probe: config.use_gpu,
        }
    }

    fn reloads(current: &Config, next: &Config) -> bool {
        current.change_to(next) >= Change::Engine
    }

    fn offset(started: Instant, at: Instant) -> Duration {
        at.saturating_duration_since(started)
    }

    fn shift(span: Duration, by: Duration) -> Duration {
        span.saturating_add(by)
    }

    fn delay(narration: Narration, by: Duration) -> Narration {
        narration.delayed(by)
    }

    fn append(text: &mut Zeroizing<String>, next: Zeroizing<String>) {
        let next = next.trim();
        if !text.is_empty() && !next.is_empty() {
            text.push(' ');
        }
        text.push_str(next);
    }
}

#[derive(Clone)]
pub(super) struct Clip {
    pub wav: Zeroizing<Vec<u8>>,
    pub language: Option<Box<str>>,
    pub audible: Duration,
}

impl Clip {
    pub(super) fn heard(mut sealed: Sealed, language: Option<Box<str>>) -> Self {
        Self {
            wav: Zeroizing::new(mem::take(&mut sealed.wav)),
            language,
            audible: sealed.audible,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Copied {
    pub ending: Ending,
    pub preservation: Preservation,
}

#[derive(Clone, Copy)]
pub(super) struct Restored {
    pub preservation: Preservation,
    pub narrated_copy: bool,
}

type Delivered = machine::Delivered<Native>;

pub(super) enum Delivery {
    Inserted(anyhow::Result<speakeasy_platform::Inserted>),
    Published(anyhow::Result<Published>),
    Abandoned(anyhow::Result<()>),
    Reopened(anyhow::Result<Box<Reopened>>),
}

impl Delivery {
    pub(super) fn settled(result: Result<Self, JoinError>) -> Delivered {
        use speakeasy_platform::Inserted;
        match result {
            Ok(Self::Inserted(Ok(Inserted::Sent))) => Delivered::Inserted(machine::Inserted::Sent),
            Ok(Self::Inserted(Ok(Inserted::Copied(message)))) => {
                Delivered::Inserted(machine::Inserted::Copied(message.into()))
            },
            Ok(Self::Inserted(Ok(Inserted::Unavailable(message)))) => {
                Delivered::Inserted(machine::Inserted::Unavailable(message.into()))
            },
            Ok(Self::Inserted(Ok(Inserted::Cancelled))) => {
                Delivered::Inserted(machine::Inserted::Cancelled)
            },
            Ok(Self::Inserted(Err(_))) => Delivered::Inserted(machine::Inserted::Failed),
            Ok(Self::Published(Ok(Published::Saved {
                show,
                ending,
                preservation,
                copied,
            }))) => Delivered::Published(machine::Published::Saved {
                shown: show,
                saved: Copied {
                    ending,
                    preservation,
                },
                copied: copied.map_err(|error| format!("{error:#}")),
            }),
            Ok(Self::Published(Ok(Published::Recovered {
                show,
                preservation,
                narrated_copy,
            }))) => Delivered::Published(machine::Published::Recovered {
                shown: show,
                recovered: Restored {
                    preservation,
                    narrated_copy,
                },
            }),
            Ok(Self::Published(Ok(Published::Cancelled))) => {
                Delivered::Published(machine::Published::Cancelled)
            },
            Ok(Self::Published(Err(error))) => {
                Delivered::Published(machine::Published::Failed(format!("{error:#}")))
            },
            Ok(Self::Abandoned(result)) => {
                Delivered::Abandoned(result.map_err(|error| format!("{error:#}")))
            },
            Ok(Self::Reopened(result)) => Delivered::Reopened(
                result
                    .map(|reopened| reopen(*reopened))
                    .map_err(|error| format!("{error:#}")),
            ),
            Err(_) => Delivered::Panicked,
        }
    }
}

fn reopen(reopened: Reopened) -> machine::Reopened<Native> {
    let Reopened {
        mut footage,
        checkpoint,
        speech,
        pending,
    } = reopened;
    let recognized = match speech {
        Some(speech) => machine::Recognized::Speech {
            audio: Clip::heard(speech.sealed, Some(speech.language)),
            start: speech.start,
            incomplete: speech.incomplete,
        },
        None => machine::Recognized::Narration {
            narration: mem::take(&mut footage.recording.narration),
            loss: footage.recording.narration_lost.map(machine_loss),
        },
    };
    machine::Reopened {
        footage: Box::new(footage),
        checkpoint: Checkpointing::Held(checkpoint),
        pending,
        recognized,
    }
}

pub(super) const fn show_loss(loss: machine::NarrationLoss) -> NarrationLoss {
    match loss {
        machine::NarrationLoss::Incomplete => NarrationLoss::Incomplete,
        machine::NarrationLoss::AudioStopped => NarrationLoss::AudioStopped,
        machine::NarrationLoss::AudioSilent => NarrationLoss::AudioSilent,
        machine::NarrationLoss::NotTranscribed => NarrationLoss::NotTranscribed,
    }
}

const fn machine_loss(loss: NarrationLoss) -> machine::NarrationLoss {
    match loss {
        NarrationLoss::Incomplete => machine::NarrationLoss::Incomplete,
        NarrationLoss::AudioStopped => machine::NarrationLoss::AudioStopped,
        NarrationLoss::AudioSilent => machine::NarrationLoss::AudioSilent,
        NarrationLoss::NotTranscribed => machine::NarrationLoss::NotTranscribed,
    }
}

pub(super) fn sealed(mut sealed: Sealed) -> machine::Sealed<Native> {
    let trimmed = sealed.trimmed;
    let partition = sealed
        .partition
        .take()
        .map(|Partition { pieces, tail }| machine::Partition {
            pieces,
            empty: tail.is_empty(),
            tail,
        });
    machine::Sealed {
        audio: Clip::heard(sealed, None),
        trimmed,
        partition,
    }
}
