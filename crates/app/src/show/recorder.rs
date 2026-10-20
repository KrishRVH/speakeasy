//! Records a Show's screen side on an owned thread: frames into a journal with their signatures,
//! and the pointer's ink and actions, publishing live ink to the overlay as it is drawn.

use std::time::{Duration, Instant};

use anyhow::anyhow;
use async_channel::{Receiver, Sender};
use speakeasy_platform::{OwnedThread, PointerMonitor, PointerSample, ScreenCapture, ScreenEvent};
use speakeasy_show::{Image, Journal, Moment, Point, Sample, Signature, Size, Tracker};
use tokio::sync::watch;

use super::overlay::Canvas;
use crate::{
    ports::{Footage, Recording},
    runtime::{SessionId, ShowEvent},
};

/// Frames are sampled this often: fast enough to catch what a click changed, slow enough that a
/// still screen costs nothing and a busy one stays cheap.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// Frames larger than this on the long edge are halved; a reading model gains nothing from more.
const LONGEST_EDGE: u32 = 2_560;
/// The journal's compressed size before it starts keeping only every other frame, then every
/// fourth, and so on, so a long scroll or video cannot exhaust memory.
const JOURNAL_BUDGET: usize = 384 << 20;
/// Frames the lanes from capture threads may hold before the newest ones are dropped.
const FRAME_LANE: usize = 2;
const POINTER_LANE: usize = 256;

/// A Show's screen recording; the session owner finishes or retires it like a microphone.
pub(crate) struct Recorder {
    /// Never sends; sending finishes the recording, and closing it cancels.
    finish: Sender<()>,
    thread: OwnedThread,
}

impl Recorder {
    /// Starts recording the display under the pointer for session `id`, whose timeline began at
    /// `started`, reporting the footage to `events` once finished.
    pub(crate) fn start(
        id: SessionId,
        started: Instant,
        events: Sender<ShowEvent>,
        canvas: watch::Sender<Canvas>,
    ) -> anyhow::Result<Self> {
        let (finish, finishing) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("show-recorder", move || {
            let outcome = record(started, &finishing, &canvas);
            canvas.send_replace(Canvas::default());
            if let Some(outcome) = outcome {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A closed event lane belongs to a departed owner; retirement still observes this thread's exit"
                )]
                let _ = events.send_blocking(ShowEvent::Finished(id, outcome));
            }
        })?;
        Ok(Self { finish, thread })
    }
}

impl Recording for Recorder {
    fn finish(&self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A full lane already holds a finish request, and a closed one belongs to an ended recording"
        )]
        let _ = self.finish.try_send(());
    }

    fn retire(self) -> impl Future<Output = ()> + Send + 'static {
        self.finish.close();
        let exited = self.thread.exited();
        async move {
            exited.await;
            drop(self);
        }
    }
}

/// Records until finished, returning the footage, or `None` once cancelled.
fn record(
    started: Instant,
    finishing: &Receiver<()>,
    canvas: &watch::Sender<Canvas>,
) -> Option<anyhow::Result<Footage>> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return Some(Err(error.into())),
    };
    runtime.block_on(Session::run(started, finishing, canvas))
}

/// The thread's state while recording.
struct Session<'a> {
    started: Instant,
    canvas: &'a watch::Sender<Canvas>,
    tracker: Tracker,
    journal: Journal,
    samples: Vec<Sample>,
    /// Every `stride`th frame is kept; it doubles whenever the journal outgrows `allowance`.
    stride: usize,
    allowance: usize,
    arrived: usize,
}

impl<'a> Session<'a> {
    async fn run(
        started: Instant,
        finishing: &Receiver<()>,
        canvas: &'a watch::Sender<Canvas>,
    ) -> Option<anyhow::Result<Footage>> {
        let (frames, frames_received) = async_channel::bounded(FRAME_LANE);
        let capture = match ScreenCapture::start(SAMPLE_INTERVAL, LONGEST_EDGE, frames) {
            Ok(capture) => capture,
            Err(error) => return Some(Err(error)),
        };
        let display = capture.display();
        let (pointer_samples, pointer_received) = async_channel::bounded(POINTER_LANE);
        let pointer = match PointerMonitor::start(display, pointer_samples) {
            Ok(pointer) => pointer,
            Err(error) => return Some(Err(error)),
        };
        canvas.send_replace(Canvas {
            display: Some(display),
            ..Canvas::default()
        });
        let [width, height] = display.size;
        let mut session = Session {
            started,
            canvas,
            tracker: Tracker::new(Size { width, height }),
            journal: Journal::new(),
            samples: Vec::new(),
            stride: 1,
            allowance: JOURNAL_BUDGET,
            arrived: 0,
        };
        let outcome = loop {
            tokio::select! {
                biased;
                finished = finishing.recv() => {
                    if finished.is_err() {
                        // The owner retired this recording: nothing is reported.
                        stop(&capture, &pointer).await;
                        return None;
                    }
                    break Ok(());
                },
                event = frames_received.recv() => match event {
                    Ok(ScreenEvent::Frame(frame)) => session.frame(frame),
                    Ok(ScreenEvent::Failed(message)) => break Err(anyhow!(message)),
                    Err(_) => break Err(anyhow!("Screen capture stopped unexpectedly. Try again.")),
                },
                Ok(sample) = pointer_received.recv() => session.pointer(sample),
            }
        };
        stop(&capture, &pointer).await;
        Some(outcome.and_then(|()| session.into_footage(Size { width, height })))
    }

    fn moment(&self, at: Instant) -> Moment {
        Moment::from(at.saturating_duration_since(self.started))
    }

    fn frame(&mut self, frame: speakeasy_platform::Frame) {
        let keep = self.arrived.is_multiple_of(self.stride);
        self.arrived = self.arrived.saturating_add(1);
        if !keep {
            return;
        }
        let at = self.moment(frame.captured);
        let Ok(image) = Image::from_bgra(frame.width, frame.height, frame.bgra) else {
            return;
        };
        self.samples.push(Sample {
            at,
            signature: Signature::of(&image),
        });
        self.journal.record(at, image);
        while self.journal.bytes() > self.allowance {
            self.stride = self.stride.saturating_mul(2);
            self.allowance = self.allowance.saturating_add(JOURNAL_BUDGET / 4);
        }
    }

    fn pointer(&mut self, sample: PointerSample) {
        let at = self.moment(sample.at);
        let strokes = self.tracker.strokes().len();
        let was_drawing = !self.tracker.drawing().is_empty();
        self.tracker.observe(
            at,
            Point::new(sample.x, sample.y),
            sample.drawing,
            sample.pressed,
        );
        let drawing: Vec<Point> = self.tracker.drawing().iter().map(|ink| ink.point).collect();
        let finished = self.tracker.strokes().get(strokes..).unwrap_or_default();
        if drawing.is_empty() && !was_drawing && finished.is_empty() {
            return;
        }
        let now = Instant::now();
        self.canvas.send_modify(|canvas| {
            if !was_drawing && !drawing.is_empty() && canvas.faded(now) {
                canvas.strokes.clear();
            }
            canvas.strokes.extend(
                finished
                    .iter()
                    .map(|stroke| stroke.points().iter().map(|ink| ink.point).collect()),
            );
            if was_drawing && drawing.is_empty() {
                canvas.lifted = Some(now);
            }
            canvas.drawing = drawing;
        });
    }

    fn into_footage(self, surface: Size) -> anyhow::Result<Footage> {
        let ended = self.moment(Instant::now());
        let (strokes, actions) = self.tracker.finish(ended);
        anyhow::ensure!(
            !self.samples.is_empty(),
            "The screen could not be recorded. Try again."
        );
        Ok(Footage {
            surface,
            ended,
            samples: self.samples,
            journal: self.journal,
            strokes,
            actions,
        })
    }
}

/// Releases the display and pointer before anything is reported, so a new Show cannot overlap
/// them.
async fn stop(capture: &ScreenCapture, pointer: &PointerMonitor) {
    capture.stop();
    pointer.stop();
    tokio::join!(capture.exited(), pointer.exited());
}
