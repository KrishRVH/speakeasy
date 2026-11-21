mod output;

use std::{
    convert::Infallible,
    mem,
    panic::{self, AssertUnwindSafe},
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

use super::{Failure, PATIENCE, Source, display_bounds, refused};
use crate::{
    Frame, Part, ScreenEvent, Target,
    macos::movie::encode::Codec,
    movie::recording::{INTERVAL, Movie},
    screen::geometry::Bounds,
};

const STOP_PAUSES: [Duration; 4] = [
    Duration::from_millis(100),
    Duration::from_millis(400),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

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
            source = Source::fetch(target.display.native.id, longest_edge) => source.map(Some),
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
    let cadence = if movie.wants_frames() {
        INTERVAL
    } else {
        interval.max(INTERVAL)
    };
    let mut stream = Stream::new(&source, target.part(), cadence)?;
    let outcome = match stream.start(stopping) {
        Ok(Started::Running) => record(
            target, interval, events, stopping, deadline, geometry, &stream, &mut movie,
        ),
        Ok(Started::Cancelled) => Ok(()),
        Err(failure) => Err(failure),
    };
    if let Err(failure) = outcome {
        report(events, failure);
    }
    let ended = Instant::now();
    if let Err(failure) = stream.stop() {
        if outcome.is_ok() {
            report(events, failure);
        }
        movie.stop(ended);
        if retry_stop(|| stream.stop(), thread::sleep).is_err() {
            report(events, Failure::Unstopped);
        }
    }
    drop(stream);
    movie.stop(ended);
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
    movie: &mut Movie<Codec>,
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
            movie_failed |= movie.frame(&frame, stopping).is_err();
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
        movie_failed |= movie.poll(Instant::now(), stopping).is_err();
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
    confirm: Sender<Infallible>,
    running: bool,
}

enum Started {
    Running,
    Cancelled,
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
        let (confirm, stopped) = async_channel::bounded(1);
        let output = output::Output::new(part, size, frames, failures, confirm.clone())?;
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
            stopped,
            confirm,
            running: false,
        })
    }

    fn start(&mut self, stopping: &Receiver<Infallible>) -> Result<Started, Failure> {
        let runtime = runtime::Builder::new_current_thread()
            .build()
            .map_err(|_runtime_error| Failure::Unstarted)?;
        let (completion, reply) = completed(Operation::Start, None);

        unsafe {
            self.native
                .startCaptureWithCompletionHandler(Some(&completion));
        }
        drop(completion);
        let started = runtime.block_on(starting(&reply, &self.stopped, stopping));
        self.running = started.is_ok();
        started
    }

    fn stop(&mut self) -> Result<(), Failure> {
        if !self.running || self.stopped.is_closed() {
            self.running = false;
            return Ok(());
        }
        let runtime = runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|_runtime_error| Failure::Stalled)?;
        let (completion, reply) = completed(Operation::Stop, Some(self.confirm.clone()));

        unsafe {
            self.native
                .stopCaptureWithCompletionHandler(Some(&completion));
        }
        drop(completion);
        let result = runtime.block_on(acknowledged(&reply, &self.stopped, PATIENCE));
        self.running = result.is_err();
        result
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.running && self.stop().is_err() {
            #[expect(
                clippy::mem_forget,
                reason = "ScreenCaptureKit never confirmed that this stream stopped; releasing its stream, output or queue could leave capture calling freed objects, so they stay retained for the process's life while the closed frame lane makes the output discard every sample"
            )]
            mem::forget((self.native.clone(), self.output.clone(), self.queue.clone()));
            return;
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

fn completed(
    operation: Operation,
    confirm: Option<Sender<Infallible>>,
) -> (Completion, Receiver<Result<(), Failure>>) {
    let (sent, reply) = async_channel::bounded(1);
    let block = RcBlock::new(move |error: *mut NSError| {
        let error = unsafe { error.as_ref() };
        let result = panic::catch_unwind(AssertUnwindSafe(|| completion_result(error, operation)))
            .unwrap_or(Err(Failure::Stalled));
        deliver(result, confirm.as_ref(), &sent);
    });
    (block, reply)
}

fn deliver(
    result: Result<(), Failure>,
    confirm: Option<&Sender<Infallible>>,
    reply: &Sender<Result<(), Failure>>,
) {
    if result.is_ok()
        && let Some(confirm) = confirm
    {
        confirm.close();
    }
    #[expect(
        clippy::let_underscore_must_use,
        reason = "There is one completion; an attempt that stopped waiting still sees a confirmed stop through the closed lane"
    )]
    let _ = reply.try_send(result);
}

async fn starting(
    reply: &Receiver<Result<(), Failure>>,
    stopped: &Receiver<Infallible>,
    stopping: &Receiver<Infallible>,
) -> Result<Started, Failure> {
    tokio::select! {
        biased;
        result = reply.recv() => result.unwrap_or(Err(Failure::Unstarted)).map(|()| Started::Running),
        _ = stopped.recv() => Err(Failure::Unstarted),
        _ = stopping.recv() => Ok(Started::Cancelled),
    }
}

async fn acknowledged(
    reply: &Receiver<Result<(), Failure>>,
    stopped: &Receiver<Infallible>,
    patience: Duration,
) -> Result<(), Failure> {
    let waited = tokio::select! {
        biased;
        _ = stopped.recv() => Ok(()),
        result = reply.recv() => result.unwrap_or(Err(Failure::Stalled)),
        () = tokio::time::sleep(patience) => Err(Failure::Stalled),
    };
    if stopped.is_closed() { Ok(()) } else { waited }
}

fn retry_stop(
    mut stop: impl FnMut() -> Result<(), Failure>,
    mut pause: impl FnMut(Duration),
) -> Result<(), Failure> {
    for wait in STOP_PAUSES {
        pause(wait);
        if stop().is_ok() {
            return Ok(());
        }
    }
    Err(Failure::Unstopped)
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
