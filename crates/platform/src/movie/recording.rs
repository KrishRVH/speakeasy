#[cfg(test)]
mod tests;

use std::{
    convert::Infallible,
    mem,
    ops::ControlFlow,
    time::{Duration, Instant},
};

use async_channel::Receiver;

use super::{
    canvas::Canvas,
    encode::{Codec, Encoder, End, Observer},
};
#[cfg(target_os = "macos")]
use crate::macos::movie::encode::Codec as Native;
#[cfg(target_os = "windows")]
use crate::windows::movie::encoder::Codec as Native;
use crate::{Frame, MovieTarget, Part};

/// One 30 Hz tick rounded up, for capture pacing only.
///
/// Sample timestamps come from `Clock::time`, not from multiples of this value.
pub(crate) const INTERVAL: Duration = Duration::from_nanos(33_333_334);

/// Caps the native encoder's disk use, independently of the retained still frames.
///
/// Stays below `MoviePrefix::FILE_LIMIT`, which readers enforce.
pub(crate) const FILE_LIMIT: u64 = 384 << 20;

pub(crate) enum Movie<C: Codec> {
    Pending(MovieTarget),
    Active(Box<Recording<C>>),
    Stopped,
}

pub(crate) struct Recording<C: Codec> {
    encoder: Option<Encoder<C>>,
    first: Instant,
    received_at: Option<Instant>,
    ending: Ending,
    /// Drops last: once its lane closes, the checkpoint may unlock or delete the movie file.
    target: MovieTarget,
}

enum Ending {
    Open,
    Stopped(Instant),
    Failed,
}

pub(crate) struct Failed;

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

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl Movie<Native> {
    pub(crate) fn new(target: Option<MovieTarget>) -> Self {
        target.map_or(Self::Stopped, Self::Pending)
    }
}

impl<C: Codec> Movie<C> {
    pub(crate) const fn wants_frames(&self) -> bool {
        !matches!(self, Self::Stopped)
    }

    /// Advances held samples and observes stop and the deadline on a quiet screen.
    ///
    /// Call it every capture tick. `Err` comes once; still capture continues after it.
    pub(crate) fn poll(
        &mut self,
        now: Instant,
        stopping: &Receiver<Infallible>,
    ) -> Result<(), Failed> {
        if self.halt(now, stopping) {
            return Ok(());
        }
        let Self::Active(recording) = self else {
            return Ok(());
        };
        let advanced = recording.advance(now, stopping);
        self.settle(advanced, now)
    }

    pub(crate) fn stop(&mut self, at: Instant) {
        self.end(Ending::Stopped(at));
    }

    pub(crate) fn frame(
        &mut self,
        frame: &Frame,
        stopping: &Receiver<Infallible>,
    ) -> Result<(), Failed> {
        *self = match mem::replace(self, Self::Stopped) {
            Self::Pending(target) => match Recording::begin(target, frame, stopping) {
                Ok(Some(recording)) => Self::Active(Box::new(recording)),
                Ok(None) => return Ok(()),
                Err(_private_native_error) => return Err(Failed),
            },
            movie => movie,
        };
        if self.halt(Instant::now(), stopping) {
            return Ok(());
        }
        let Self::Active(recording) = self else {
            return Ok(());
        };
        let written = recording.frame(frame, stopping);
        self.settle(written, Instant::now())
    }

    fn halt(&mut self, now: Instant, stopping: &Receiver<Infallible>) -> bool {
        let target = match self {
            Self::Pending(target) => target,
            Self::Active(recording) => &recording.target,
            Self::Stopped => return true,
        };
        let halted = target.cancelled() || stopping.is_closed();
        if halted {
            let ending = now.min(target.deadline());
            self.stop(ending);
        }
        halted
    }

    fn settle(
        &mut self,
        outcome: anyhow::Result<ControlFlow<()>>,
        now: Instant,
    ) -> Result<(), Failed> {
        match outcome {
            Ok(ControlFlow::Continue(())) => Ok(()),
            Ok(ControlFlow::Break(())) => {
                self.stop(now);
                Ok(())
            },
            Err(_private_native_error) => {
                self.end(Ending::Failed);
                Err(Failed)
            },
        }
    }

    fn end(&mut self, ending: Ending) {
        if let Self::Active(recording) = self {
            recording.ending = ending;
        }
        *self = Self::Stopped;
    }
}

impl<C: Codec> Recording<C> {
    fn begin(
        target: MovieTarget,
        frame: &Frame,
        stopping: &Receiver<Infallible>,
    ) -> anyhow::Result<Option<Self>> {
        let canvas = Canvas::new(frame.image.size())?;
        if target.cancelled() || !target.begin(frame.at, canvas.size(), stopping) {
            return Ok(None);
        }
        let encoder = Encoder::open(target.path(), canvas, FILE_LIMIT)?;
        Ok(Some(Self {
            encoder: Some(encoder),
            first: frame.at,
            received_at: None,
            ending: Ending::Open,
            target,
        }))
    }

    fn frame(
        &mut self,
        frame: &Frame,
        stopping: &Receiver<Infallible>,
    ) -> anyhow::Result<ControlFlow<()>> {
        let at = frame.at.saturating_duration_since(self.first);
        anyhow::ensure!(
            frame.at < self.target.deadline() && at <= self.target.limit(),
            "The Show movie reached its recording limit"
        );
        if self
            .received_at
            .is_some_and(|previous| frame.at <= previous)
        {
            return Ok(ControlFlow::Continue(()));
        }
        let Some(encoder) = &mut self.encoder else {
            return Ok(ControlFlow::Continue(()));
        };
        let target = &self.target;
        let written = encoder.write(frame, at, &mut Publication { target, stopping })?;
        if written.is_continue() {
            self.received_at = Some(frame.at);
        }
        Ok(written)
    }

    fn advance(
        &mut self,
        now: Instant,
        stopping: &Receiver<Infallible>,
    ) -> anyhow::Result<ControlFlow<()>> {
        let through = now
            .min(self.horizon())
            .saturating_duration_since(self.first);
        let Some(encoder) = &mut self.encoder else {
            return Ok(ControlFlow::Continue(()));
        };
        let target = &self.target;
        encoder.advance(End::Through(through), &mut Publication { target, stopping })
    }

    /// The session deadline or the first frame plus the limit, whichever comes first.
    ///
    /// Held samples on a quiet screen and the reported coverage both stop here.
    fn horizon(&self) -> Instant {
        let deadline = self.target.deadline();
        self.first
            .checked_add(self.target.limit())
            .map_or(deadline, |limit| limit.min(deadline))
    }
}

impl<C: Codec> Drop for Recording<C> {
    fn drop(&mut self) {
        let coverage = self.encoder.as_ref().and_then(Encoder::coverage);
        let last = coverage
            .and_then(|coverage| self.first.checked_add(coverage.last))
            .unwrap_or(self.first);
        let presented = coverage
            .and_then(|coverage| self.first.checked_add(coverage.at))
            .unwrap_or(self.first);
        let ended = match self.ending {
            Ending::Stopped(at) => Some(at),
            Ending::Open | Ending::Failed => self.received_at,
        };
        let through = ended
            .unwrap_or(self.first)
            .max(presented)
            .min(self.horizon());
        // A failed recording skips finalize: the macOS writer raises a fatal Objective-C
        // exception when finishing after a failure. Coverage is reported either way, since
        // fragments already written stay playable.
        let finalized = self.encoder.take().is_some_and(|encoder| {
            !matches!(self.ending, Ending::Failed) && encoder.finish().is_ok()
        });
        self.target.finish(
            last,
            through,
            coverage.map_or(0, |coverage| coverage.frames),
            finalized,
        );
    }
}
