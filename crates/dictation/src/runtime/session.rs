//! Authority and presentation stage for one recording. Retired native work belongs to the owner, so
//! abandoning a session never waits or opens its replacement.

use std::time::Instant;

use speakeasy_core::gesture::State;
use speakeasy_platform::{InsertPermit, Inserted};
use tokio::task::JoinHandle;

use super::{Phase, timeline::Timeline};

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

pub(super) type InsertTask = JoinHandle<anyhow::Result<Inserted>>;

pub(super) struct Session {
    pub id: SessionId,
    pub permit: InsertPermit,
    pub level: f32,
    pub meter_tick: u64,
    pub stage: Stage,
    pub timeline: Timeline,
}

impl Session {
    pub(super) const fn new(id: SessionId, permit: InsertPermit, pressed: Instant) -> Self {
        Self {
            id,
            permit,
            level: 0.0,
            meter_tick: 0,
            stage: Stage::Queued,
            timeline: Timeline::new(pressed),
        }
    }

    pub(super) const fn phase(&self) -> Phase {
        match self.stage {
            Stage::Queued | Stage::Opening => Phase::Starting,
            Stage::Recording => Phase::Recording,
            Stage::Stopping => Phase::Stopping,
            Stage::AwaitingWorker(_) | Stage::Transcribing | Stage::Inserting(_) => {
                Phase::Processing
            },
        }
    }

    pub(super) const fn is_capturing(&self) -> bool {
        matches!(
            self.stage,
            Stage::Opening | Stage::Recording | Stage::Stopping
        )
    }

    pub(super) fn insertion(&mut self) -> Option<&mut InsertTask> {
        if let Stage::Inserting(task) = &mut self.stage {
            Some(task)
        } else {
            None
        }
    }

    /// Starts recording once samples arrive, or stops at once when the gesture already finished.
    pub(super) fn audio_ready(&mut self, gesture: State) {
        if matches!(self.stage, Stage::Opening) {
            self.stage = if gesture == State::Processing {
                Stage::Stopping
            } else {
                Stage::Recording
            };
        }
    }

    pub(super) fn show_level(&mut self, level: f32) {
        self.level = level;
        self.meter_tick = self.meter_tick.wrapping_add(1);
    }

    pub(super) fn finish(&mut self) {
        if matches!(self.stage, Stage::Recording) {
            self.stage = Stage::Stopping;
        }
    }
}

pub(super) enum Stage {
    /// Waits until any previous capture has released the microphone.
    Queued,
    Opening,
    Recording,
    /// The gesture has finished; waits for the capture's audio.
    Stopping,
    /// Captured WAV awaiting a ready worker; zeroed, best effort, if the session is abandoned.
    AwaitingWorker(Vec<u8>),
    Transcribing,
    Inserting(InsertTask),
}
