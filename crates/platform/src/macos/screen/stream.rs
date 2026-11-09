mod output;

use std::{
    convert::Infallible,
    panic::{self, AssertUnwindSafe},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{AllocAnyThread as _, rc::Retained, runtime::ProtocolObject};
use objc2_core_graphics::{CGDirectDisplayID, CGDisplayPixelsHigh, CGDisplayPixelsWide};
use objc2_core_media::CMTime;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCStream, SCStreamErrorCode, SCStreamErrorDomain, SCStreamOutputType,
};
use tokio::runtime;

use super::{Failure, PATIENCE, Source, answer, display_bounds, refused};
use crate::{
    Frame, Part, ScreenEvent, Target,
    movie::recording::{INTERVAL, Movie},
    screen::geometry::Bounds,
};

pub(super) fn capture(
    target: Target,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
    target_movie: Option<crate::MovieTarget>,
) -> Result<(), Failure> {
    let deadline = target_movie.as_ref().map(crate::MovieTarget::deadline);
    let mut movie = Movie::new(target_movie);
    let runtime = runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|_runtime_error| Failure::Unstarted)?;
    let source = runtime.block_on(async {
        tokio::select! {
            _ = stopping.recv() => Ok(None),
            () = events.closed() => Ok(None),
            source = answer(Source::fetch(target.display.native.id, longest_edge)) => {
                source.unwrap_or(Err(Failure::Stalled)).map(Some)
            },
        }
    })?;
    let Some(source) = source else {
        return Ok(());
    };
    if stopping.is_closed() || events.is_closed() {
        return Ok(());
    }
    let geometry = Geometry::read(target.display.native.id);
    if geometry.bounds != target.display.native.bounds {
        return Err(Failure::DisplayLost);
    }
    let cadence = if movie.active() {
        INTERVAL
    } else {
        interval.max(INTERVAL)
    };
    let mut stream = Stream::new(&source, target.part(), cadence)?;
    let outcome = stream.start().and_then(|()| {
        record(
            target, interval, events, stopping, deadline, geometry, &stream, &mut movie,
        )
    });
    if let Err(failure) = outcome {
        report(events, failure);
    }
    let ended = Instant::now();
    match stream.stop() {
        Ok(()) => {
            drop(stream);
            movie.stop(ended);
        },
        Err(failure) => {
            if outcome.is_ok() {
                report(events, failure);
            }
            movie.stop(ended);
            drop(stream);
        },
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "This private loop borrows one capture's immutable policy and its two native owners"
)]
fn record(
    target: Target,
    interval: Duration,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
    deadline: Option<Instant>,
    geometry: Geometry,
    stream: &Stream,
    movie: &mut Movie,
) -> Result<(), Failure> {
    let started = Instant::now();
    let mut last_frame = None;
    let mut last_still = None;
    loop {
        let now = Instant::now();
        if stopping.is_closed() || events.is_closed() || deadline.is_some_and(|end| now >= end) {
            return Ok(());
        }
        if let Ok(failure) = stream.failures.try_recv() {
            return Err(failure);
        }
        if Geometry::read(target.display.native.id) != geometry {
            return Err(Failure::DisplayLost);
        }
        let mut movie_failed = false;
        while let Ok(frame) = stream.frames.try_recv() {
            if Geometry::read(target.display.native.id) != geometry {
                return Err(Failure::DisplayLost);
            }
            if stopping.is_closed() || events.is_closed() {
                return Ok(());
            }
            if last_frame.is_some_and(|previous| frame.at <= previous) {
                continue;
            }
            last_frame = Some(frame.at);
            movie_failed |= !movie.frame(&frame, stopping);
            if last_still
                .is_none_or(|previous| frame.at.saturating_duration_since(previous) >= interval)
            {
                last_still = Some(frame.at);
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "The bounded still lane may drop an image; closure ends the owning loop"
                )]
                let _ = events.try_send(ScreenEvent::Frame(frame));
            }
        }
        if last_frame.is_none() && started.elapsed() >= PATIENCE {
            return Err(Failure::Stalled);
        }
        movie_failed |= !movie.poll(Instant::now(), stopping);
        if movie_failed {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A movie failure replaces a queued still so a full lane cannot hide it"
            )]
            let _ = events.force_send(ScreenEvent::MovieStopped);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Geometry {
    bounds: Bounds,
    pixels: [usize; 2],
}

impl Geometry {
    fn read(display: CGDirectDisplayID) -> Self {
        Self {
            bounds: display_bounds(display),
            pixels: [CGDisplayPixelsWide(display), CGDisplayPixelsHigh(display)],
        }
    }
}

struct Stream {
    native: Retained<SCStream>,
    output: Retained<output::Output>,
    queue: DispatchRetained<DispatchQueue>,
    frames: Receiver<Frame>,
    failures: Receiver<Failure>,
    stopped: Receiver<Infallible>,
    running: bool,
}

impl Stream {
    fn new(source: &Source, part: Part, interval: Duration) -> Result<Self, Failure> {
        let configuration = source.configuration(part).ok_or(Failure::DisplayLost)?;
        let nanos = i64::try_from(interval.as_nanos()).map_err(|_overflow| Failure::Unstarted)?;

        let size = unsafe {
            configuration.setMinimumFrameInterval(CMTime::new(nanos, 1_000_000_000));
            configuration.setQueueDepth(3);
            configuration.setCapturesAudio(false);
            [configuration.width(), configuration.height()]
        };
        let (frames, received) = async_channel::bounded(2);
        let (failures, failed) = async_channel::bounded(1);
        let (stopped, terminated) = async_channel::bounded(1);
        let output = output::Output::new(part, size, frames, failures, stopped)?;
        let queue = DispatchQueue::new("speakeasy.screen", None);

        let native = unsafe {
            let native = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &source.filter,
                &configuration,
                Some(ProtocolObject::from_ref(&*output)),
            );
            native
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|_private_error| Failure::Unstarted)?;
            native
        };
        Ok(Self {
            native,
            output,
            queue,
            frames: received,
            failures: failed,
            stopped: terminated,
            running: false,
        })
    }

    fn start(&mut self) -> Result<(), Failure> {
        self.running = true;
        let (completion, reply) = completed(Operation::Start);

        unsafe {
            self.native
                .startCaptureWithCompletionHandler(Some(&completion));
        }
        reply.recv().unwrap_or(Err(Failure::Unstarted))
    }

    fn stop(&mut self) -> Result<(), Failure> {
        if !self.running || self.stopped.is_closed() {
            self.running = false;
            return Ok(());
        }
        let (completion, reply) = completed(Operation::Stop);

        unsafe {
            self.native
                .stopCaptureWithCompletionHandler(Some(&completion));
        }
        let result = reply.recv().unwrap_or(Err(Failure::Stalled));
        if result.is_ok() || self.stopped.is_closed() {
            self.running = false;
        }
        result
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        while self.running {
            if self.stop().is_err() {
                thread::sleep(Duration::from_millis(100));
            }
        }
        #[expect(
            clippy::let_underscore_must_use,
            reason = "An already stopped native stream may report no output; queue drainage remains mandatory"
        )]
        let _ = unsafe {
            self.native.removeStreamOutput_type_error(
                ProtocolObject::from_ref(&*self.output),
                SCStreamOutputType::Screen,
            )
        };
        self.queue.exec_sync(|| {});
    }
}

type Completion = RcBlock<dyn Fn(*mut NSError)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Start,
    Stop,
}

fn completed(operation: Operation) -> (Completion, mpsc::Receiver<Result<(), Failure>>) {
    let (sent, reply) = mpsc::sync_channel(1);
    let block = RcBlock::new(move |error: *mut NSError| {
        let error = unsafe { error.as_ref() };
        let result = panic::catch_unwind(AssertUnwindSafe(|| completion_result(error, operation)))
            .unwrap_or(Err(Failure::Stalled));
        #[expect(
            clippy::let_underscore_must_use,
            reason = "There is one completion; a departed owner cannot receive its result"
        )]
        let _ = sent.send(result);
    });
    (block, reply)
}

fn completion_result(error: Option<&NSError>, operation: Operation) -> Result<(), Failure> {
    let Some(error) = error else {
        return Ok(());
    };

    let domain = unsafe { SCStreamErrorDomain };
    if operation == Operation::Stop
        && *error.domain() == *domain
        && error.code() == SCStreamErrorCode::AttemptToStopStreamState.0
    {
        return Ok(());
    }
    Err(if refused(error) {
        Failure::Permission
    } else {
        Failure::Stalled
    })
}

fn report(events: &Sender<ScreenEvent>, failure: Failure) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A terminal failure replaces a queued still before cleanup can wait on native acknowledgment"
    )]
    let _ = events.force_send(failure.into());
}

#[cfg(test)]
mod tests;
