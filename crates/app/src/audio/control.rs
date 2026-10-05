//! Capture-local synchronization for callbacks that cannot wait on the owner.

use std::{
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    thread,
};

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
        reason = "The consumer seals audio before stopping the device, so it must wait for in-flight callback publication without blocking the callback"
    )]
    publishing: AtomicUsize,
}

/// One in-flight callback. Dropping the guard acknowledges its samples and errors.
pub(super) struct Publishing<'a>(&'a Control);

impl Drop for Publishing<'_> {
    fn drop(&mut self) {
        self.0.publishing.fetch_sub(1, Ordering::SeqCst);
    }
}

// Mode and publication share a sequentially consistent order: callbacks count themselves before
// reading the mode, and the consumer finishes before waiting for their count to reach zero.
impl Control {
    pub(super) fn mode(&self) -> Mode {
        const RECORDING: u8 = Mode::Recording as u8;
        const FINISHING: u8 = Mode::Finishing as u8;
        match self.mode.load(Ordering::SeqCst) {
            RECORDING => Mode::Recording,
            FINISHING => Mode::Finishing,
            _ => Mode::Cancelled,
        }
    }

    pub(super) fn finish(&self) {
        self.mode.fetch_max(Mode::Finishing as u8, Ordering::SeqCst);
    }

    pub(super) fn cancel(&self) {
        self.mode.fetch_max(Mode::Cancelled as u8, Ordering::SeqCst);
    }

    /// Counts a callback before it checks whether recording has stopped.
    pub(super) fn publishing(&self) -> Publishing<'_> {
        self.publishing.fetch_add(1, Ordering::SeqCst);
        Publishing(self)
    }

    /// After finishing, waits for callbacks that may still publish their last packet.
    pub(super) fn quiesce(&self) {
        while self.publishing.load(Ordering::SeqCst) != 0 {
            thread::yield_now();
        }
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
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };

    use super::*;

    #[test]
    fn quiescence_waits_for_a_callback_that_began_before_the_finish() {
        let control = Arc::new(Control::default());
        let (opened, open) = mpsc::channel();
        let (close, closing) = mpsc::channel::<()>();
        let callback = thread::spawn({
            let control = control.clone();
            move || {
                let _publishing = control.publishing();
                opened.send(control.mode()).unwrap();
                closing.recv().unwrap();
            }
        });
        assert_eq!(
            open.recv_timeout(Duration::from_secs(2)).unwrap(),
            Mode::Recording
        );
        control.finish();
        let (quiesced, done) = mpsc::channel();
        let consumer = thread::spawn(move || {
            control.quiesce();
            quiesced.send(()).unwrap();
        });
        let premature = done.recv_timeout(Duration::from_millis(50)).is_ok();
        // Release the callback and join even when the assertion under test fails.
        close.send(()).unwrap();
        callback.join().unwrap();
        consumer.join().unwrap();
        assert!(!premature, "Quiesced while a callback was still publishing");
        done.recv_timeout(Duration::from_secs(2)).unwrap();
    }
}
