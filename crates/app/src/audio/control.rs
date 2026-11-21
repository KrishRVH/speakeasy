use std::{
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
    thread,
};

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
    #[expect(
        clippy::disallowed_types,
        reason = "The consumer seals audio before stopping the device, so it must wait for in-flight callback publication without blocking the callback"
    )]
    publishing: AtomicUsize,
}

#[must_use = "a callback counts as publishing only while its guard lives"]
pub(super) struct Publishing<'a>(&'a Control);

impl Drop for Publishing<'_> {
    fn drop(&mut self) {
        self.0.publishing.fetch_sub(1, Ordering::SeqCst);
    }
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

    pub(super) fn publishing(&self) -> Publishing<'_> {
        self.publishing.fetch_add(1, Ordering::SeqCst);
        Publishing(self)
    }

    pub(super) fn quiesce(&self) {
        while self.publishing.load(Ordering::SeqCst) != 0 {
            thread::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };

    use super::*;
    use crate::testing::PATIENCE;

    #[test]
    fn quiescence_waits_for_a_callback_that_began_before_the_finish() {
        let control = Arc::new(Control::default());
        let (opened, open) = mpsc::channel();
        let (close, closing) = mpsc::channel::<()>();
        let callback = thread::spawn({
            let control = Arc::clone(&control);
            move || {
                let _publishing = control.publishing();
                opened.send(control.mode()).unwrap();
                closing.recv().unwrap();
            }
        });
        assert_eq!(open.recv_timeout(PATIENCE).unwrap(), Mode::Recording);
        control.finish();
        let (quiesced, done) = mpsc::channel();
        let consumer = thread::spawn(move || {
            control.quiesce();
            quiesced.send(()).unwrap();
        });
        let premature = done.recv_timeout(Duration::from_millis(50)).is_ok();

        close.send(()).unwrap();
        callback.join().unwrap();
        consumer.join().unwrap();
        assert!(!premature, "Quiesced while a callback was still publishing");
        done.recv_timeout(PATIENCE).unwrap();
    }
}
