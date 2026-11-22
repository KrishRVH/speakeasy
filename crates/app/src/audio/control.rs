use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Recording,
    Finishing,
    Cancelled,
}

const FINISH: u8 = 1;
const CANCEL: u8 = 2;

#[derive(Default)]
pub(super) struct Control {
    #[expect(
        clippy::disallowed_types,
        reason = "The audio callback reads finish/cancel without waiting on the session owner"
    )]
    mode: AtomicU8,
}

impl Control {
    pub(super) fn mode(&self) -> Mode {
        let requested = self.mode.load(Ordering::SeqCst);
        if requested & CANCEL != 0 {
            Mode::Cancelled
        } else if requested & FINISH != 0 {
            Mode::Finishing
        } else {
            Mode::Recording
        }
    }

    pub(super) fn finished(&self) -> bool {
        self.mode.load(Ordering::SeqCst) & FINISH != 0
    }

    pub(super) fn finish(&self) {
        self.mode.fetch_or(FINISH, Ordering::SeqCst);
    }

    pub(super) fn cancel(&self) {
        self.mode.fetch_or(CANCEL, Ordering::SeqCst);
    }
}
