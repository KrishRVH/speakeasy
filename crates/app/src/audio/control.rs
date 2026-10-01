//! Capture-local synchronization for callbacks that cannot wait on the owner.
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Recording,
    Finishing,
    Cancelled,
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
    overran: AtomicBool,
    #[expect(
        clippy::disallowed_types,
        reason = "Native data and error callbacks distinguish a startup discontinuity from lost recorded speech"
    )]
    started: AtomicBool,
}

impl Control {
    pub(super) fn mode(&self) -> Mode {
        match self.mode.load(Ordering::Acquire) {
            0 => Mode::Recording,
            1 => Mode::Finishing,
            _ => Mode::Cancelled,
        }
    }

    pub(super) fn finish(&self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Finish must not replace an existing cancel; a failed exchange means finish or cancel already won"
        )]
        let _ = self
            .mode
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(super) fn cancel(&self) {
        self.mode.store(2, Ordering::Release);
    }

    pub(super) fn overrun(&self) {
        self.overran.store(true, Ordering::Release);
    }

    pub(super) fn overran(&self) -> bool {
        self.overran.load(Ordering::Acquire)
    }

    pub(super) fn start(&self) {
        self.started.store(true, Ordering::Release);
    }

    pub(super) fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }
}
