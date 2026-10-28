//! Records a Show's screen side on an owned thread: frames into a journal with their signatures,
//! and the pointer's ink, pins, and actions, publishing live ink and numbered marks to the
//! overlay as they are made. An area Show first has the user drag out its area on the overlay.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use async_channel::{Receiver, Sender};
use speakeasy_platform::{
    Display, OwnedThread, Part, PointerMonitor, PointerSample, Scope, ScreenCapture, ScreenEvent,
    Taps, Target,
};
use speakeasy_show::{Image, Journal, Mark, Moment, Point, SAMPLE_INTERVAL, Size, Span, Tracker};
use tokio::sync::watch;

use super::overlay::Canvas;
use crate::runtime::ports::{Capturing, Footage, Pinning, SessionId, ShowEvent};

/// Frames larger than this on the long edge are halved; a reading model gains nothing from more.
pub(super) const LONGEST_EDGE: u32 = 2_560;
/// The most compressed bytes a Show's journal holds; see [`Journal::with_budget`].
const JOURNAL_BUDGET: usize = 384 << 20;
/// How long a Show finished before its first frame waits for one; a Show needs the screen.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(3);
/// How many frames wait for the recorder; [`ScreenCapture::start`] says what capture does with a
/// frame the lane has no room for.
const FRAME_LANE: usize = 2;
/// How many pointer samples wait for the recorder. A sample that finds the lane full is skipped.
const POINTER_LANE: usize = 256;
/// How many pins wait for the recorder. A pin that finds the lane full is dropped.
const PIN_LANE: usize = 8;
/// The failure a recording reports when capture or the recorder itself stops without an outcome.
const CAPTURE_STOPPED: &str = "Screen capture stopped unexpectedly. Try again.";

/// A Show's screen recording; the session owner finishes or retires it like a microphone.
pub(crate) struct ScreenRecorder {
    /// Sending finishes the recording; closing it cancels.
    finish: Sender<()>,
    /// The double taps of Shift that asked for pins.
    pins: Sender<Taps>,
    thread: OwnedThread,
}

impl ScreenRecorder {
    /// Starts recording what `scope` names, near the pointer, for session `id`, whose timeline
    /// began at `started`, publishing the display, live ink, and marks to `canvas` and the
    /// footage to `events` once finished.
    pub(crate) fn start(
        id: SessionId,
        started: Instant,
        scope: Scope,
        events: Sender<ShowEvent>,
        canvas: watch::Sender<Canvas>,
    ) -> anyhow::Result<Self> {
        let (finish, finishing) = async_channel::bounded(1);
        let (pins, pinned) = async_channel::bounded(PIN_LANE);
        let thread = OwnedThread::spawn("show-recorder", move || {
            let lanes = Lanes {
                finishing,
                pinned,
                canvas: &canvas,
            };
            let mut ending = Ending {
                id,
                events,
                canvas: &canvas,
                outcome: None,
            };
            ending.outcome = Some(record(started, scope, &lanes));
        })?;
        Ok(Self {
            finish,
            pins,
            thread,
        })
    }
}

impl Capturing for ScreenRecorder {
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

impl Pinning for ScreenRecorder {
    fn pin(&self, taps: Taps) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A full lane means pins faster than anyone reads them, and a closed one belongs to an ended recording"
        )]
        let _ = self.pins.try_send(taps);
    }
}

/// A recorder dropped without retiring cancels before its thread joins.
impl Drop for ScreenRecorder {
    fn drop(&mut self) {
        self.finish.close();
    }
}

/// Ends a recording however its thread leaves it: when dropped, it hides the overlay and reports
/// `outcome` to the owner, or a fixed failure once recording unwinds without one, since a panic's
/// message could carry private text. A cancelled recording reports nothing.
struct Ending<'a> {
    id: SessionId,
    events: Sender<ShowEvent>,
    canvas: &'a watch::Sender<Canvas>,
    /// How recording ended; `None` while it runs.
    outcome: Option<anyhow::Result<Option<Footage>>>,
}

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.canvas.send_replace(Canvas::Hidden);
        let outcome = self
            .outcome
            .take()
            .unwrap_or_else(|| Err(anyhow!(CAPTURE_STOPPED)));
        if let Some(outcome) = outcome.transpose() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A closed event lane belongs to a departed owner; retirement still observes this thread's exit"
            )]
            let _ = self
                .events
                .send_blocking(ShowEvent::Finished(self.id, outcome));
        }
    }
}

/// What reaches the recording thread from the owner, and where it publishes to the overlay.
struct Lanes<'a> {
    /// A message finishes the recording; closing cancels it.
    finishing: Receiver<()>,
    pinned: Receiver<Taps>,
    canvas: &'a watch::Sender<Canvas>,
}

/// Records until finished, returning the footage, or `None` once cancelled.
fn record(started: Instant, scope: Scope, lanes: &Lanes<'_>) -> anyhow::Result<Option<Footage>> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?
        .block_on(async {
            let Some((target, finished)) = choose(scope, lanes).await? else {
                return Ok(None);
            };
            Take::run(started, target, finished, lanes).await
        })
}

/// Resolves what `scope` names into a target, and whether the Show was finished meanwhile; `None`
/// once cancelled. An area is dragged out on the overlay over the display under the pointer; a
/// Show finished before then, or whose overlay cannot appear, records all of that display.
async fn choose(scope: Scope, lanes: &Lanes<'_>) -> anyhow::Result<Option<(Target, bool)>> {
    let display = match scope {
        Scope::Screen => return Target::screen().map(|target| Some((target, false))),
        Scope::Window => return Target::window_under_pointer().map(|target| Some((target, false))),
        Scope::Area => Display::under_pointer()?,
    };
    let (chosen, choice) = async_channel::bounded(1);
    lanes
        .canvas
        .send_replace(Canvas::Picking { display, chosen });
    Ok(pick(&choice, lanes)
        .await
        .map(|(part, finished)| (Target::area(display, part), finished)))
}

/// Waits for the part of the display dragged out on `choice`, or all of it when the overlay closes
/// `choice` because it cannot appear, or all of it, finished, when the Show finishes first; `None`
/// once cancelled.
async fn pick(choice: &Receiver<Part>, lanes: &Lanes<'_>) -> Option<(Part, bool)> {
    let picked = tokio::select! {
        biased;
        finished = lanes.finishing.recv() => finished.ok().map(|()| (Part::WHOLE, true)),
        part = choice.recv() => Some((part.unwrap_or(Part::WHOLE), false)),
    };
    // A pin asked for before there was anything to record marks nothing.
    while lanes.pinned.try_recv().is_ok() {}
    picked
}

/// The thread's state while recording.
struct Take<'a> {
    started: Instant,
    canvas: &'a watch::Sender<Canvas>,
    /// The part of the display the latest frame showed, which the pointer is placed in.
    part: Part,
    tracker: Tracker,
    journal: Journal,
}

impl<'a> Take<'a> {
    /// Records `target` until finished, or from the start when `finished`.
    async fn run(
        started: Instant,
        target: Target,
        finished: bool,
        lanes: &Lanes<'a>,
    ) -> anyhow::Result<Option<Footage>> {
        let (frames, frames_received) = async_channel::bounded(FRAME_LANE);
        let capture = ScreenCapture::start(target, SAMPLE_INTERVAL, LONGEST_EDGE, frames)?;
        let display = target.display();
        let (pointer_samples, pointer_received) = async_channel::bounded(POINTER_LANE);
        let pointer = PointerMonitor::start(display, pointer_samples)?;
        let part = target.part();
        lanes.canvas.send_replace(Canvas::covering(display, part));
        let [width, height] = display.size;
        let mut take = Take {
            started,
            canvas: lanes.canvas,
            part,
            tracker: Tracker::new(Size {
                width: width * part.width(),
                height: height * part.height(),
            }),
            journal: Journal::with_budget(JOURNAL_BUDGET),
        };
        // Set once finished: the latest moment to keep waiting for a first frame.
        let mut finished_by = finished.then(first_frame_deadline);
        let outcome = loop {
            tokio::select! {
                biased;
                finished = lanes.finishing.recv() => {
                    if finished.is_err() {
                        // The owner retired this recording: nothing is reported.
                        stop(&capture, &pointer).await;
                        return Ok(None);
                    }
                    if !take.journal.is_empty() {
                        break Ok(());
                    }
                    finished_by.get_or_insert_with(first_frame_deadline);
                },
                () = tokio::time::sleep_until(finished_by.unwrap_or_else(tokio::time::Instant::now)),
                    if finished_by.is_some() => break Ok(()),
                event = frames_received.recv() => match event {
                    Ok(ScreenEvent::Frame(frame)) => {
                        take.frame(frame);
                        if finished_by.is_some() && !take.journal.is_empty() {
                            break Ok(());
                        }
                    },
                    Ok(ScreenEvent::Failed(message)) => break Err(anyhow!(message)),
                    Err(_) => break Err(anyhow!(CAPTURE_STOPPED)),
                },
                // Samples first, so a pin lands where the pointer is and sees the taps that asked
                // for it.
                Ok(sample) = pointer_received.recv() => take.pointer(sample),
                Ok(taps) = lanes.pinned.recv() => take.pin(taps),
            }
        };
        stop(&capture, &pointer).await;
        outcome?;
        take.into_footage().map(Some)
    }

    fn moment(&self, at: Instant) -> Moment {
        Moment::from(at.saturating_duration_since(self.started))
    }

    fn frame(&mut self, frame: speakeasy_platform::Frame) {
        if frame.part != self.part {
            self.part = frame.part;
            self.canvas.send_if_modified(|canvas| {
                canvas.sheet().is_some_and(|sheet| sheet.track(frame.part))
            });
        }
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
        let [x, y] = self.part.within([sample.x, sample.y]);
        self.tracker
            .observe(at, Point::new(x, y), sample.drawing, sample.pressed);
        let finished: Vec<Arc<[Point]>> = self
            .tracker
            .strokes()
            .get(strokes..)
            .unwrap_or_default()
            .iter()
            .map(|stroke| stroke.points().iter().map(|ink| ink.point).collect())
            .collect();
        let marks = (!finished.is_empty()).then(|| self.marks());
        let drawing: Vec<Point> = self.tracker.drawing().iter().map(|ink| ink.point).collect();
        let now = Instant::now();
        self.canvas.send_if_modified(|canvas| {
            canvas
                .sheet()
                .is_some_and(|sheet| sheet.update(finished, drawing, marks, now))
        });
    }

    fn pin(&mut self, taps: Taps) {
        let taps = Span::new(self.moment(taps.pressed), self.moment(taps.released));
        if !self.tracker.pin(taps) {
            return;
        }
        let marks = Some(self.marks());
        let drawing: Vec<Point> = self.tracker.drawing().iter().map(|ink| ink.point).collect();
        let now = Instant::now();
        self.canvas.send_if_modified(|canvas| {
            canvas
                .sheet()
                .is_some_and(|sheet| sheet.update([], drawing, marks, now))
        });
    }

    fn marks(&self) -> Arc<[Mark]> {
        self.tracker.marks().into()
    }

    fn into_footage(self) -> anyhow::Result<Footage> {
        let ended = self.moment(Instant::now());
        anyhow::ensure!(
            !self.journal.is_empty(),
            "The screen could not be recorded. Try again."
        );
        Ok(Footage {
            recording: self.tracker.finish(ended),
            journal: self.journal.sealed(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_area_is_dragged_out_or_recorded_whole_and_earlier_pins_mark_nothing()
    -> anyhow::Result<()> {
        let (finish, finishing) = async_channel::bounded(1);
        let (pins, pinned) = async_channel::bounded(PIN_LANE);
        let (canvas, _) = watch::channel(Canvas::default());
        let lanes = Lanes {
            finishing,
            pinned,
            canvas: &canvas,
        };
        let (chosen, choice) = async_channel::bounded(1);
        let dragged = Part::between([0.1, 0.2], [0.5, 0.6]);
        let now = Instant::now();
        pins.try_send(Taps {
            pressed: now,
            released: now,
        })?;
        chosen.try_send(dragged)?;
        assert_eq!(pick(&choice, &lanes).await, Some((dragged, false)));
        assert!(lanes.pinned.is_empty(), "the early pin was dropped");
        finish.try_send(())?;
        assert_eq!(pick(&choice, &lanes).await, Some((Part::WHOLE, true)));
        chosen.close();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), pick(&choice, &lanes)).await?,
            Some((Part::WHOLE, false)),
            "the overlay could not appear"
        );
        finish.close();
        assert_eq!(pick(&choice, &lanes).await, None, "cancelled");
        Ok(())
    }

    #[test]
    fn a_recording_that_unwinds_hides_the_overlay_and_reports_a_fixed_failure() -> anyhow::Result<()>
    {
        let (events, reported) = async_channel::bounded(1);
        let (canvas, overlay) = watch::channel(Canvas::default());
        let unwound = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _ending = Ending {
                        id: SessionId::FIRST,
                        events,
                        canvas: &canvas,
                        outcome: None,
                    };
                    panic!("a recording bug");
                })
                .join()
        });
        assert!(unwound.is_err());
        assert!(overlay.has_changed()?, "the overlay was told to hide");
        let ShowEvent::Finished(id, outcome) = reported.try_recv()?;
        assert_eq!(id, SessionId::FIRST);
        assert_eq!(
            outcome.err().map(|error| error.to_string()).as_deref(),
            Some(CAPTURE_STOPPED)
        );
        Ok(())
    }
}

/// Linux records no Shows.
#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
mod native_tests {
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
        let recorder =
            ScreenRecorder::start(SessionId::FIRST, started, Scope::Screen, events, canvas)?;
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
        assert_eq!(footage.recording.surface, Size { width, height });
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
        let ended = footage.recording.duration.since(Moment::START);
        assert!(
            (Duration::from_millis(1_200)..Duration::from_secs(5)).contains(&ended),
            "{ended:?}"
        );
        assert!(
            moments
                .last()
                .is_some_and(|last| *last <= footage.recording.duration)
        );
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
        let recorder = ScreenRecorder::start(
            SessionId::FIRST,
            Instant::now(),
            Scope::Screen,
            events,
            canvas,
        )?;
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
