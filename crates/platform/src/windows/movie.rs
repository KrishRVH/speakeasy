//! Silent H.264 fragments on the capture thread. Only already admitted, cropped frames enter
//! this writer. The checkpoint lease outlives native teardown, including failed finalization.

mod canvas;
mod encoder;
mod file;

use std::{
    convert::Infallible,
    mem,
    time::{Duration, Instant},
};

use encoder::Encoder;

use crate::{Frame, MovieTarget};

pub(super) const INTERVAL: Duration = Duration::from_nanos(33_333_334);
/// Bounds the native encoder's disk use independently of retained still frames.
const FILE_LIMIT: u64 = 384 << 20;
const MAX_FRAMES: u32 = 9_001;

#[cfg(test)]
mod tests;

pub(super) enum Movie {
    Pending(MovieTarget),
    Active(Recording),
    Stopped,
}

pub(super) struct Recording {
    encoder: Option<Encoder>,
    first: Instant,
    last: Instant,
    frames: u32,
    ending: Option<Instant>,
    // Last to drop: a receiver observing closure may immediately unlock or delete the checkpoint.
    target: MovieTarget,
}

impl Movie {
    pub(super) fn new(target: Option<MovieTarget>) -> Self {
        target.map_or(Self::Stopped, Self::Pending)
    }

    pub(super) const fn active(&self) -> bool {
        !matches!(self, Self::Stopped)
    }

    /// Cancellation must be observed even when the desktop produces no new frames.
    pub(super) fn poll(&mut self, now: Instant, stopping: &async_channel::Receiver<Infallible>) {
        let target = match self {
            Self::Pending(target) => target,
            Self::Active(recording) => &recording.target,
            Self::Stopped => return,
        };
        if target.cancelled() || stopping.is_closed() {
            let ending = now.min(target.deadline());
            self.stop(ending);
        }
    }

    pub(super) fn stop(&mut self, at: Instant) {
        if let Self::Active(recording) = self {
            recording.ending = Some(at);
        }
        *self = Self::Stopped;
    }

    /// False reports a codec/storage failure once. The caller can keep recording keyframes.
    pub(super) fn frame(
        &mut self,
        frame: &Frame,
        stopping: &async_channel::Receiver<Infallible>,
    ) -> bool {
        if matches!(self, Self::Pending(_)) {
            let Self::Pending(target) = mem::replace(self, Self::Stopped) else {
                return true;
            };
            match Recording::begin(target, frame, stopping) {
                Ok(Some(recording)) => *self = Self::Active(recording),
                Ok(None) => return true,
                Err(_private_native_error) => return false,
            }
        }
        if let Self::Active(recording) = self {
            if recording.target.cancelled() || stopping.is_closed() {
                *self = Self::Stopped;
            } else if recording.frame(frame).is_err() {
                *self = Self::Stopped;
                return false;
            }
        }
        true
    }
}

impl Recording {
    fn begin(
        target: MovieTarget,
        frame: &Frame,
        stopping: &async_channel::Receiver<Infallible>,
    ) -> anyhow::Result<Option<Self>> {
        let canvas = canvas::Canvas::new([frame.width, frame.height])?;
        if target.cancelled() || !target.begin(frame.at, canvas.size(), stopping) {
            return Ok(None);
        }
        let encoder = Encoder::open(target.path(), canvas, FILE_LIMIT)?;
        Ok(Some(Self {
            encoder: Some(encoder),
            first: frame.at,
            last: frame.at,
            frames: 0,
            ending: None,
            target,
        }))
    }

    fn frame(&mut self, frame: &Frame) -> anyhow::Result<()> {
        let at = frame.at.saturating_duration_since(self.first);
        anyhow::ensure!(
            frame.at < self.target.deadline()
                && at <= Duration::from_secs(300)
                && self.frames < MAX_FRAMES,
            "The Show movie reached its recording limit"
        );
        if frame.at < self.last || (self.frames > 0 && frame.at == self.last) {
            return Ok(());
        }
        if let Some(encoder) = &mut self.encoder {
            encoder.write(frame, at)?;
            self.last = frame.at;
            self.frames = self.frames.saturating_add(1);
        }
        Ok(())
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let through = self
            .ending
            .unwrap_or(self.last)
            .max(self.last)
            .min(self.target.deadline());
        let ending = self.ending.map(|at| {
            at.min(self.target.deadline())
                .saturating_duration_since(self.first)
                .min(Duration::from_secs(300))
        });
        let finalized = self
            .encoder
            .take()
            .is_some_and(|encoder| encoder.finish(ending).is_ok());
        self.target
            .finish(self.last, through, self.frames, finalized);
    }
}
