#[cfg(test)]
mod tests;

use std::{
    convert::Infallible,
    mem,
    time::{Duration, Instant},
};

use async_channel::Receiver;

use super::{
    RECORDING_LIMIT, canvas,
    encode::{Encoder as Timeline, End, Observer},
};
#[cfg(target_os = "macos")]
use crate::macos::movie::encode::Codec;
#[cfg(target_os = "windows")]
use crate::windows::movie::encoder::Codec;
use crate::{Frame, MovieTarget, Part};

type Encoder = Timeline<Codec>;

pub(crate) const INTERVAL: Duration = Duration::from_nanos(33_333_334);

const FILE_LIMIT: u64 = 384 << 20;

pub(crate) enum Movie {
    Pending(MovieTarget),
    Active(Box<Recording>),
    Stopped,
}

pub(crate) struct Recording {
    encoder: Option<Encoder>,
    first: Instant,
    received_at: Option<Instant>,
    ending: Option<Instant>,
    failed: bool,
    target: MovieTarget,
}

struct Publication<'a> {
    target: &'a MovieTarget,
    stopping: &'a Receiver<Infallible>,
}

impl Observer for Publication<'_> {
    fn cancelled(&mut self) -> bool {
        self.target.cancelled() || self.stopping.is_closed()
    }

    fn geometry(&mut self, sample: u32, content: [u32; 4], part: Part) -> anyhow::Result<bool> {
        self.target.geometry(sample, content, part, self.stopping)
    }
}

impl Movie {
    pub(crate) fn new(target: Option<MovieTarget>) -> Self {
        target.map_or(Self::Stopped, Self::Pending)
    }

    pub(crate) const fn active(&self) -> bool {
        !matches!(self, Self::Stopped)
    }

    pub(crate) fn poll(&mut self, now: Instant, stopping: &Receiver<Infallible>) -> bool {
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
        let Self::Active(recording) = self else {
            return true;
        };
        let advanced = recording.advance(now, stopping);
        self.settle(advanced, now)
    }

    pub(crate) fn stop(&mut self, at: Instant) {
        if let Self::Active(recording) = self {
            recording.ending = Some(at);
        }
        *self = Self::Stopped;
    }

    pub(crate) fn frame(&mut self, frame: &Frame, stopping: &Receiver<Infallible>) -> bool {
        *self = match mem::replace(self, Self::Stopped) {
            Self::Pending(target) => match Recording::begin(target, frame, stopping) {
                Ok(Some(recording)) => Self::Active(Box::new(recording)),
                Ok(None) => return true,
                Err(_private_native_error) => return false,
            },
            movie => movie,
        };
        let Self::Active(recording) = self else {
            return true;
        };
        if recording.target.cancelled() || stopping.is_closed() {
            let ending = Instant::now().min(recording.target.deadline());
            self.stop(ending);
            return true;
        }
        let written = recording.frame(frame, stopping);
        self.settle(written, Instant::now())
    }

    fn settle(&mut self, outcome: anyhow::Result<bool>, now: Instant) -> bool {
        match outcome {
            Ok(true) => true,
            Ok(false) => {
                self.stop(now);
                true
            },
            Err(_private_native_error) => {
                if let Self::Active(recording) = self {
                    recording.failed = true;
                }
                *self = Self::Stopped;
                false
            },
        }
    }
}

impl Recording {
    fn begin(
        target: MovieTarget,
        frame: &Frame,
        stopping: &Receiver<Infallible>,
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

    fn frame(&mut self, frame: &Frame, stopping: &Receiver<Infallible>) -> anyhow::Result<bool> {
        let at = frame.at.saturating_duration_since(self.first);
        anyhow::ensure!(
            frame.at < self.target.deadline() && at <= RECORDING_LIMIT,
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

    fn advance(&mut self, now: Instant, stopping: &Receiver<Infallible>) -> anyhow::Result<bool> {
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
