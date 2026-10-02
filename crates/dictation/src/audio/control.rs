//! Capture-local synchronization for callbacks that cannot wait on the owner.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Modes only advance in declaration order, so a late finish cannot undo a cancel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Mode {
    Recording = 0,
    Finishing = 1,
    Cancelled = 2,
}

#[derive(Default)]
pub(super) struct Control {
    #[expect(
        clippy::disallowed_types,
        reason = "The audio callback reads finish/cancel without waiting on the session owner"
    )]
    mode: AtomicU8,
    #[expect(
        clippy::disallowed_types,
        reason = "The callback reports overflow to the owned PCM consumer without allocating or waiting"
    )]
    overflowed: AtomicBool,
    #[expect(
        clippy::disallowed_types,
        reason = "Native data and error callbacks distinguish a startup discontinuity from lost recorded speech"
    )]
    first_sample_queued: AtomicBool,
    #[expect(
        clippy::disallowed_types,
        reason = "The owner enables pause excerpts for the running consumer, which it reaches only through this shared control"
    )]
    speculating: AtomicBool,
}

impl Control {
    pub(super) fn mode(&self) -> Mode {
        const RECORDING: u8 = Mode::Recording as u8;
        const FINISHING: u8 = Mode::Finishing as u8;
        match self.mode.load(Ordering::Acquire) {
            RECORDING => Mode::Recording,
            FINISHING => Mode::Finishing,
            _ => Mode::Cancelled,
        }
    }

    pub(super) fn finish(&self) {
        self.mode.fetch_max(Mode::Finishing as u8, Ordering::AcqRel);
    }

    pub(super) fn cancel(&self) {
        self.mode.fetch_max(Mode::Cancelled as u8, Ordering::AcqRel);
    }

    pub(super) fn mark_overflowed(&self) {
        self.overflowed.store(true, Ordering::Release);
    }

    pub(super) fn overflowed(&self) -> bool {
        self.overflowed.load(Ordering::Acquire)
    }

    pub(super) fn mark_first_sample_queued(&self) {
        self.first_sample_queued.store(true, Ordering::Release);
    }

    pub(super) fn first_sample_queued(&self) -> bool {
        self.first_sample_queued.load(Ordering::Acquire)
    }

    /// Asks the consumer to offer the audio up to each pause for speculative recognition.
    pub(super) fn speculate(&self) {
        self.speculating.store(true, Ordering::Release);
    }

    pub(super) fn speculating(&self) -> bool {
        self.speculating.load(Ordering::Acquire)
    }
}
