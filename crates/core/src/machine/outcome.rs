use super::{Payload, Phase};

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Outcome<P: Payload> {
    Idle,
    NothingToPaste,
    Done,
    Practiced,
    Copied(P::Text),
    ShowCopied(P::Saved),
    ShowGone,
    Recovered(P::Recovered),
    Empty,
    Cancelled,
    Unchosen(P::Unchosen),
    Failed(Failure<P>),
}

impl<P: Payload> Outcome<P> {
    pub const fn phase(&self) -> Phase {
        match self {
            Self::Idle => Phase::Idle,
            Self::Done | Self::Practiced => Phase::Done,
            Self::Copied(_) | Self::ShowCopied(_) => Phase::Copied,
            Self::NothingToPaste | Self::ShowGone | Self::Recovered(_) => Phase::Notice,
            Self::Empty => Phase::Empty,
            Self::Cancelled => Phase::Cancelled,
            Self::Unchosen(_) => Phase::Unchosen,
            Self::Failed(_) => Phase::Error,
        }
    }
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Failure<P: Payload> {
    Said(P::Text),
    InputLost,
    Undelivered {
        said: Option<P::Text>,
        replay: bool,
    },
    PublishStopped,
    NotCopied {
        saved: P::Saved,
        error: P::Text,
        failure: Option<P::Text>,
    },
    RecoveryStorage(P::Text),
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Notice<P: Payload> {
    CheckpointLost,
    MovieStopped,
    LoadingModel,
    SettingsApplied,
    WarmupFailed(P::Text),
    NarrationLost(P::Text),
}

impl<P: Payload> Notice<P> {
    pub const fn failure(&self) -> Option<&P::Text> {
        match self {
            Self::WarmupFailed(failure) | Self::NarrationLost(failure) => Some(failure),
            Self::LoadingModel
            | Self::SettingsApplied
            | Self::CheckpointLost
            | Self::MovieStopped => None,
        }
    }

    pub const fn is_warning(&self) -> bool {
        matches!(self, Self::CheckpointLost | Self::MovieStopped)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    Busy,
    NotReady,
    StartStopped,
    NotActive,
    PasteBusy,
    PasteNotReady,
    NothingToPaste,
    PasteStopped,
    Unsupported,
    PresetBusy,
    PresetStale,
}

#[cfg(not(test))]
redacted!(Outcome, Failure, Notice);
