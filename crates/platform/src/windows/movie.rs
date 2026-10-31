//! Silent H.264 fragments on the capture thread. Only already admitted, cropped frames enter
//! this writer. The checkpoint lease outlives native teardown, including failed finalization.

mod canvas;
mod encoder;
mod file;
mod foundation;
pub(super) mod read;

use std::{
    convert::Infallible,
    mem,
    time::{Duration, Instant},
};

use encoder::{Encoder, End, Observer};

use crate::{Frame, MovieTarget};

pub(super) const INTERVAL: Duration = Duration::from_nanos(33_333_334);
/// Bounds the native encoder's disk use independently of retained still frames.
const FILE_LIMIT: u64 = 384 << 20;

#[cfg(test)]
mod tests;

pub(super) enum Movie {
    Pending(MovieTarget),
    Active(Box<Recording>),
    Stopped,
}

pub(super) struct Recording {
    encoder: Option<Encoder>,
    first: Instant,
    received_at: Option<Instant>,
    ending: Option<Instant>,
    failed: bool,
    // Last to drop: a receiver observing closure may immediately unlock or delete the checkpoint.
    target: MovieTarget,
}

struct Publication<'a> {
    target: &'a MovieTarget,
    stopping: &'a async_channel::Receiver<Infallible>,
}

impl Observer for Publication<'_> {
    fn cancelled(&mut self) -> bool {
        self.target.cancelled() || self.stopping.is_closed()
    }

    fn geometry(
        &mut self,
        sample: u32,
        content: [u32; 4],
        part: crate::Part,
    ) -> anyhow::Result<bool> {
        self.target.geometry(sample, content, part, self.stopping)
    }
}

impl Movie {
    pub(super) fn new(target: Option<MovieTarget>) -> Self {
        target.map_or(Self::Stopped, Self::Pending)
    }

    pub(super) const fn active(&self) -> bool {
        !matches!(self, Self::Stopped)
    }

    /// Advances held frames and observes cancellation even on a quiet desktop. False reports
    /// a codec/storage failure once, while the caller continues retaining still frames.
    pub(super) fn poll(
        &mut self,
        now: Instant,
        stopping: &async_channel::Receiver<Infallible>,
    ) -> bool {
        let target = match self {
            Self::Pending(target) => target,
            Self::Active(recording) => &recording.target,
            Self::Stopped => return true,
        };
        if target.cancelled() || stopping.is_closed() {
            let ending = now.min(target.deadline());
            self.stop(ending);
            return true;
        }
        if let Self::Active(recording) = self {
            match recording.advance(now, stopping) {
                Ok(true) => {},
                Ok(false) => self.stop(now),
                Err(_private_native_error) => {
                    recording.failed = true;
                    *self = Self::Stopped;
                    return false;
                },
            }
        }
        true
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
                Ok(Some(recording)) => *self = Self::Active(Box::new(recording)),
                Ok(None) => return true,
                Err(_private_native_error) => return false,
            }
        }
        if let Self::Active(recording) = self {
            if recording.target.cancelled() || stopping.is_closed() {
                let end = Instant::now().min(recording.target.deadline());
                self.stop(end);
            } else {
                match recording.frame(frame, stopping) {
                    Ok(true) => {},
                    Ok(false) => self.stop(Instant::now()),
                    Err(_private_native_error) => {
                        recording.failed = true;
                        *self = Self::Stopped;
                        return false;
                    },
                }
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
            received_at: None,
            ending: None,
            failed: false,
            target,
        }))
    }

    fn frame(
        &mut self,
        frame: &Frame,
        stopping: &async_channel::Receiver<Infallible>,
    ) -> anyhow::Result<bool> {
        let at = frame.at.saturating_duration_since(self.first);
        anyhow::ensure!(
            frame.at < self.target.deadline() && at <= Duration::from_secs(300),
            "The Show movie reached its recording limit"
        );
        if self
            .received_at
            .is_some_and(|previous| frame.at <= previous)
        {
            return Ok(true);
        }
        if let Some(encoder) = &mut self.encoder {
            if encoder
                .write(
                    frame,
                    at,
                    &mut Publication {
                        target: &self.target,
                        stopping,
                    },
                )?
                .is_break()
            {
                return Ok(false);
            }
            self.received_at = Some(frame.at);
        }
        Ok(true)
    }

    fn advance(
        &mut self,
        now: Instant,
        stopping: &async_channel::Receiver<Infallible>,
    ) -> anyhow::Result<bool> {
        let target = &self.target;
        if let Some(encoder) = &mut self.encoder {
            return Ok(encoder
                .advance(
                    End::Through(
                        now.min(target.deadline())
                            .saturating_duration_since(self.first),
                    ),
                    &mut Publication { target, stopping },
                )?
                .is_continue());
        }
        Ok(true)
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let coverage = self.encoder.as_ref().and_then(Encoder::coverage);
        let last = coverage
            .and_then(|coverage| self.first.checked_add(coverage.last))
            .unwrap_or(self.first);
        let presented = coverage
            .and_then(|coverage| self.first.checked_add(coverage.at))
            .unwrap_or(self.first);
        let through = self
            .ending
            .or(self.received_at)
            .unwrap_or(self.first)
            .max(presented)
            .min(self.target.deadline());
        let finalized = self
            .encoder
            .take()
            .is_some_and(|encoder| encoder.finish().is_ok())
            && !self.failed;
        self.target.finish(
            last,
            through,
            coverage.map_or(0, |coverage| coverage.frames),
            finalized,
        );
    }
}
