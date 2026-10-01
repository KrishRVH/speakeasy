//! Resources and authority for one recording. Retired native work belongs to
//! the owner, so abandoning a session never waits or opens its replacement.
use super::Phase;
use speakeasy_platform::{InsertPermit, Inserted};
use std::time::Instant;
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct SessionId(u64);

impl SessionId {
    #[cfg(test)]
    pub(crate) const FIRST: Self = Self(1);

    pub(super) fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    pub(super) fn get(self) -> u64 {
        self.0
    }
}

pub(super) type InsertTask = JoinHandle<anyhow::Result<Inserted>>;

pub(super) struct Session {
    pub id: SessionId,
    pub started: Instant,
    pub permit: InsertPermit,
    pub level: f32,
    pub meter_tick: u64,
    pub stage: Stage,
}

pub(super) enum Stage {
    Queued { microphone: Option<String> },
    Opening,
    Recording,
    Stopping,
    AwaitingWorker(Vec<u8>),
    Transcribing,
    Inserting(InsertTask),
}

impl Session {
    pub(super) fn phase(&self) -> Phase {
        match self.stage {
            Stage::Queued { .. } | Stage::Opening => Phase::Starting,
            Stage::Recording => Phase::Recording,
            Stage::Stopping => Phase::Stopping,
            Stage::AwaitingWorker(_) | Stage::Transcribing | Stage::Inserting(_) => {
                Phase::Processing
            },
        }
    }

    pub(super) fn capturing(&self) -> bool {
        matches!(
            self.stage,
            Stage::Opening | Stage::Recording | Stage::Stopping
        )
    }

    pub(super) fn ready(mut self, finishing: bool) -> Self {
        self.stage = match self.stage {
            Stage::Opening if finishing => Stage::Stopping,
            Stage::Opening => Stage::Recording,
            stage => stage,
        };
        self
    }

    pub(super) fn finish(mut self) -> Self {
        self.stage = match self.stage {
            Stage::Recording => Stage::Stopping,
            stage => stage,
        };
        self
    }
}
