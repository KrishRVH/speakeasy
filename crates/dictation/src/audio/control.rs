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
        reason = "The owner enables pause excerpts for the running consumer, which it reaches only through this shared control"
    )]
    speculating: AtomicBool,
    #[expect(
        clippy::disallowed_types,
        reason = "The consumer must learn that no callback is still publishing after a finish, and the callback cannot wait"
    )]
    publishing: AtomicUsize,
    #[expect(
        clippy::disallowed_types,
        reason = "The consumer asks the callback to wake it once audio it waits for is queued, and the callback cannot wait"
    )]
    wake_at: AtomicUsize,
}

/// A callback inside its publishing section, which ends when this drops.
pub(super) struct Publishing<'a>(&'a Control);

impl Drop for Publishing<'_> {
    fn drop(&mut self) {
        self.0.publishing.fetch_sub(1, Ordering::SeqCst);
    }
}

// The mode and the publishing count are sequentially consistent: a callback counts itself before it
// reads the mode, and `quiesce` reads the count after the finish, so any callback that missed the
// finish is still counted.
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

    /// Opens a callback's publishing section; it must precede the callback's mode check.
    pub(super) fn publishing(&self) -> Publishing<'_> {
        self.publishing.fetch_add(1, Ordering::SeqCst);
        Publishing(self)
    }

    /// After a finish, waits until no callback that began before it is still publishing. A section
    /// spans one packet's downmix: microseconds.
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

    /// Asks the callback to wake the consumer once `samples` have been queued since capture began, or
    /// not at all.
    pub(super) fn wake_at(&self, samples: Option<usize>) {
        self.wake_at
            .store(samples.unwrap_or(usize::MAX), Ordering::Release);
    }

    /// Whether `queued` samples reach the requested wake, which this clears so the consumer is
    /// woken once per request.
    pub(super) fn reached_wake(&self, queued: usize) -> bool {
        let target = self.wake_at.load(Ordering::Acquire);
        queued >= target
            && self
                .wake_at
                .compare_exchange(target, usize::MAX, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    /// Asks the consumer to offer the audio up to each pause for speculative recognition.
    pub(super) fn speculate(&self) {
        self.speculating.store(true, Ordering::Release);
    }

    pub(super) fn speculating(&self) -> bool {
        self.speculating.load(Ordering::Acquire)
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
    fn a_requested_wake_is_reached_once_and_only_once_its_audio_is_queued() {
        let control = Control::default();
        control.wake_at(Some(480));
        assert!(!control.reached_wake(479));
        assert!(control.reached_wake(512));
        assert!(
            !control.reached_wake(1024),
            "One request woke the consumer twice"
        );
        control.wake_at(None);
        assert!(!control.reached_wake(usize::MAX - 1));
    }

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
        assert_eq!(open.recv().unwrap(), Mode::Recording);
        control.finish();
        let (quiesced, done) = mpsc::channel();
        let consumer = thread::spawn(move || {
            control.quiesce();
            quiesced.send(()).unwrap();
        });
        assert!(
            done.recv_timeout(Duration::from_millis(50)).is_err(),
            "Quiesced while a callback was still publishing"
        );
        close.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        callback.join().unwrap();
        consumer.join().unwrap();
    }
}
