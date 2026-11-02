use std::{ops::ControlFlow, path::Path, time::Duration};

use anyhow::ensure;

use super::canvas::Canvas;
use crate::Frame;

const FRAME_LIMIT: u32 = 9_000;
const CATCH_UP_LIMIT: u32 = 60;

pub(crate) trait Codec: Sized {
    const CLOCK: Clock;
    fn open(path: &Path, size: [u32; 2], limit: u64) -> anyhow::Result<Self>;
    fn write(
        &mut self,
        pixels: &[u8],
        index: u32,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<bool>;
    fn checkpoint(&mut self, at: Duration) -> anyhow::Result<()>;
    fn finish(self) -> anyhow::Result<()>;
}

pub(crate) enum End {
    Before(Duration),
    Through(Duration),
}

pub(crate) trait Observer {
    fn cancelled(&mut self) -> bool;

    fn geometry(
        &mut self,
        sample: u32,
        content: [u32; 4],
        part: crate::Part,
    ) -> anyhow::Result<bool>;
}

#[cfg(test)]
impl<F: FnMut() -> bool> Observer for F {
    fn cancelled(&mut self) -> bool {
        self()
    }

    fn geometry(&mut self, _: u32, _: [u32; 4], _: crate::Part) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Coverage {
    pub(crate) last: Duration,
    pub(crate) at: Duration,
    pub(crate) frames: u32,
}

pub(crate) struct Encoder<C> {
    codec: C,
    canvas: Canvas,
    capture: Option<Duration>,
    coverage: Option<Coverage>,
    geometry: Option<([u32; 4], crate::Part)>,
}

impl<C: Codec> Encoder<C> {
    pub(crate) fn open(path: &Path, canvas: Canvas, limit: u64) -> anyhow::Result<Self> {
        let codec = C::open(path, canvas.size(), limit)?;
        Ok(Self {
            codec,
            canvas,
            capture: None,
            coverage: None,
            geometry: None,
        })
    }

    pub(crate) fn write(
        &mut self,
        frame: &Frame,
        at: Duration,
        observer: &mut impl Observer,
    ) -> anyhow::Result<ControlFlow<()>> {
        let at = C::CLOCK.quantize(at);
        ensure!(
            at <= Duration::from_secs(300)
                && self
                    .capture
                    .map_or_else(|| at.is_zero(), |previous| at >= previous),
            "Movie capture time is invalid"
        );

        if self.advance(End::Before(at), observer)?.is_break() || observer.cancelled() {
            return Ok(ControlFlow::Break(()));
        }
        self.canvas.paint(frame)?;
        self.capture = Some(at);
        self.advance(End::Through(at), observer)
    }

    pub(crate) fn advance(
        &mut self,
        end: End,
        observer: &mut impl Observer,
    ) -> anyhow::Result<ControlFlow<()>> {
        let Some(capture) = self.capture else {
            return Ok(ControlFlow::Continue(()));
        };
        let end = match end {
            End::Before(at) => End::Before(C::CLOCK.quantize(at)),
            End::Through(at) => End::Through(C::CLOCK.quantize(at)),
        };
        let first = self.coverage.map_or(0, |coverage| coverage.frames);
        let mut index = first;
        while index < FRAME_LIMIT
            && match end {
                End::Before(at) => C::CLOCK.time(index) < at,
                End::Through(at) => C::CLOCK.time(index) <= at,
            }
        {
            if observer.cancelled() {
                return Ok(ControlFlow::Break(()));
            }
            ensure!(
                index.saturating_sub(first) < CATCH_UP_LIMIT,
                "Video encoding fell behind; retained frames remain available"
            );
            let at = C::CLOCK.time(index);
            let content = self.canvas.content();
            let part = self.canvas.part();
            if self.geometry != Some((content, part)) {
                if !observer.geometry(index, content, part)? || observer.cancelled() {
                    return Ok(ControlFlow::Break(()));
                }
                self.geometry = Some((content, part));
            }
            if !self
                .codec
                .write(self.canvas.held(), index, &mut || observer.cancelled())?
            {
                return Ok(ControlFlow::Break(()));
            }
            index = index.saturating_add(1);
            self.coverage = Some(Coverage {
                last: capture,
                at,
                frames: index,
            });
            self.codec.checkpoint(at)?;
        }
        Ok(ControlFlow::Continue(()))
    }

    pub(crate) const fn coverage(&self) -> Option<Coverage> {
        self.coverage
    }

    pub(crate) fn finish(self) -> anyhow::Result<()> {
        self.codec.finish()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Clock {
    #[cfg(target_os = "macos")]
    Nanoseconds,
    #[cfg(target_os = "windows")]
    HundredNanoseconds,
}

impl Clock {
    fn quantize(self, at: Duration) -> Duration {
        match self {
            #[cfg(target_os = "macos")]
            Self::Nanoseconds => at,
            #[cfg(target_os = "windows")]
            Self::HundredNanoseconds => {
                Duration::new(at.as_secs(), (at.subsec_nanos() / 100).saturating_mul(100))
            },
        }
    }

    pub(crate) fn time(self, index: u32) -> Duration {
        let (ticks, unit) = match self {
            #[cfg(target_os = "macos")]
            Self::Nanoseconds => (1_000_000_000, 1),
            #[cfg(target_os = "windows")]
            Self::HundredNanoseconds => (10_000_000, 100),
        };
        Duration::from_nanos(
            u64::from(index)
                .saturating_mul(ticks)
                .saturating_div(30)
                .saturating_mul(unit),
        )
    }
}
