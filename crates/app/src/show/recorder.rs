//! Records a Show's screen side on an owned thread: frames into a journal with their signatures,
//! and the pointer's ink and actions, publishing live ink to the overlay as it is drawn.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use async_channel::{Receiver, Sender};
use speakeasy_platform::{OwnedThread, PointerMonitor, PointerSample, ScreenCapture, ScreenEvent};
use speakeasy_show::{Image, Journal, Moment, Point, SAMPLE_INTERVAL, Size, Tracker};
use tokio::sync::watch;

use super::overlay::Canvas;
use crate::runtime::ports::{Footage, Recording, SessionId, ShowEvent};

/// Frames larger than this on the long edge are halved; a reading model gains nothing from more.
pub(super) const LONGEST_EDGE: u32 = 2_560;
/// The most compressed bytes a Show's journal holds; see [`Journal::with_budget`].
const JOURNAL_BUDGET: usize = 384 << 20;
/// How long a Show finished before its first frame waits for one; a Show needs the screen.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(3);
/// Frames the lanes from capture threads may hold before the newest ones are dropped.
const FRAME_LANE: usize = 2;
const POINTER_LANE: usize = 256;

/// A Show's screen recording; the session owner finishes or retires it like a microphone.
pub(crate) struct Recorder {
    /// Sending finishes the recording; closing it cancels.
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

/// A recorder dropped without retiring cancels before its thread joins.
impl Drop for Recorder {
    fn drop(&mut self) {
        self.finish.close();
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
        canvas.send_replace(Canvas::covering(display));
        let [width, height] = display.size;
        let mut session = Session {
            started,
            canvas,
            tracker: Tracker::new(Size { width, height }),
            journal: Journal::with_budget(JOURNAL_BUDGET),
        };
        // Set once finished: the latest moment to keep waiting for a first frame.
        let mut finished_by = None;
        let outcome = loop {
            tokio::select! {
                biased;
                finished = finishing.recv() => {
                    if finished.is_err() {
                        // The owner retired this recording: nothing is reported.
                        stop(&capture, &pointer).await;
                        return None;
                    }
                    if !session.journal.is_empty() {
                        break Ok(());
                    }
                    finished_by.get_or_insert_with(first_frame_deadline);
                },
                () = tokio::time::sleep_until(finished_by.unwrap_or_else(tokio::time::Instant::now)),
                    if finished_by.is_some() => break Ok(()),
                event = frames_received.recv() => match event {
                    Ok(ScreenEvent::Frame(frame)) => {
                        session.frame(frame);
                        if finished_by.is_some() && !session.journal.is_empty() {
                            break Ok(());
                        }
                    },
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
        if !self.journal.admit() {
            return;
        }
        let at = self.moment(frame.captured);
        if let Ok(image) = Image::from_bgra(frame.width, frame.height, frame.bgra) {
            self.journal.record(at, image);
        }
    }

    fn pointer(&mut self, sample: PointerSample) {
        let at = self.moment(sample.at);
        let strokes = self.tracker.strokes().len();
        self.tracker.observe(
            at,
            Point::new(sample.x, sample.y),
            sample.drawing,
            sample.pressed,
        );
        let finished: Vec<Arc<[Point]>> = self
            .tracker
            .strokes()
            .get(strokes..)
            .unwrap_or_default()
            .iter()
            .map(|stroke| stroke.points().iter().map(|ink| ink.point).collect())
            .collect();
        let drawing: Vec<Point> = self.tracker.drawing().iter().map(|ink| ink.point).collect();
        let now = Instant::now();
        self.canvas
            .send_if_modified(|canvas| canvas.update(finished, drawing, now));
    }

    fn into_footage(self, surface: Size) -> anyhow::Result<Footage> {
        let ended = self.moment(Instant::now());
        let (strokes, actions) = self.tracker.finish(ended);
        anyhow::ensure!(
            !self.journal.is_empty(),
            "The screen could not be recorded. Try again."
        );
        Ok(Footage {
            surface,
            ended,
            journal: self.journal.sealed(),
            strokes,
            actions,
        })
    }
}

fn first_frame_deadline() -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(FIRST_FRAME_WAIT).unwrap_or(now)
}

/// Releases the display and pointer before anything is reported, so a new Show cannot overlap
/// them.
async fn stop(capture: &ScreenCapture, pointer: &PointerMonitor) {
    capture.request_stop();
    pointer.request_stop();
    tokio::join!(capture.stopped(), pointer.stopped());
}

/// Linux records no Shows yet.
#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
mod tests {
    use anyhow::Context as _;
    use tokio::time::timeout;

    use super::*;

    /// Records the real screen briefly and checks only the footage's shape: no pixel is compared,
    /// written, or printed.
    #[tokio::test]
    #[ignore = "Records the real screen for about a second and keeps no pixels; native opt-in"]
    async fn a_real_screen_records_footage_on_one_timeline() -> anyhow::Result<()> {
        let (events, finished) = async_channel::bounded(1);
        let (canvas, ink) = watch::channel(Canvas::default());
        let started = Instant::now();
        let recorder = Recorder::start(SessionId::FIRST, started, events, canvas)?;
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        let display = ink
            .borrow()
            .display()
            .context("The overlay was given a display")?;
        recorder.finish();
        let ShowEvent::Finished(id, footage) =
            timeout(Duration::from_secs(10), finished.recv()).await??;
        assert_eq!(id, SessionId::FIRST);
        let footage = footage?;
        timeout(Duration::from_secs(5), recorder.retire()).await?;
        assert!(ink.borrow().display().is_none(), "The overlay was released");

        let [width, height] = display.size;
        assert_eq!(footage.surface, Size { width, height });
        let moments: Vec<Moment> = footage
            .journal
            .samples()
            .iter()
            .map(|sample| sample.at)
            .collect();
        assert!(
            (4..=8).contains(&moments.len()),
            "{} frames in 1.3 s at one per 200 ms",
            moments.len()
        );
        assert!(
            moments.windows(2).all(|pair| pair[0] < pair[1]),
            "{moments:?}"
        );
        let ended = footage.ended.since(Moment::START);
        assert!(
            (Duration::from_millis(1_200)..Duration::from_secs(5)).contains(&ended),
            "{ended:?}"
        );
        assert!(moments.last().is_some_and(|last| *last <= footage.ended));
        let frame = footage
            .journal
            .frame_at(Moment::START)
            .context("A first frame")?;
        assert!(frame.width().max(frame.height()) <= LONGEST_EDGE);
        let (aspect, surface) = (frame.aspect(), width / height);
        assert!((aspect - surface).abs() < 0.02, "{aspect} vs {surface}");
        Ok(())
    }

    /// Records the real screen for ten seconds so a process sampler can measure what recording
    /// costs; it reports only the footage's shape.
    #[tokio::test]
    #[ignore = "Records the real screen for ten seconds and keeps no pixels; native opt-in profiling"]
    async fn profile_real_screen_recording() -> anyhow::Result<()> {
        let (events, finished) = async_channel::bounded(1);
        let (canvas, _ink) = watch::channel(Canvas::default());
        let recorder = Recorder::start(SessionId::FIRST, Instant::now(), events, canvas)?;
        tokio::time::sleep(Duration::from_secs(10)).await;
        recorder.finish();
        let ShowEvent::Finished(_, footage) =
            timeout(Duration::from_secs(10), finished.recv()).await??;
        let footage = footage?;
        timeout(Duration::from_secs(5), recorder.retire()).await?;
        eprintln!(
            "show_recording: seconds=10 frames={} journal_mb={:.1}",
            footage.journal.samples().len(),
            footage.journal.bytes() as f64 / 1e6
        );
        Ok(())
    }
}
