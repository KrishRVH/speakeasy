//! A Show's screen recorder, on an owned thread. It journals frames with their signatures and
//! records the pointer's ink, pins, and actions, publishing live ink and numbered marks to the
//! overlay as they are made. An area Show first has the user drag out its area on the overlay. A
//! Show records only what its scope names: with no window or area to record, it records nothing.

use std::{
    mem,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use async_channel::{Receiver, Sender};
use speakeasy_platform::{
    Display, Frame, OwnedThread, Part, PointerMonitor, PointerSample, Scope, ScreenCapture,
    ScreenEvent, Taps, Target, WindowGone,
};
use speakeasy_show::{Ending, Image, Mark, Moment, Point, SAMPLE_INTERVAL, Size, Span, TakeEvent};
use tokio::sync::watch;

use super::{FrameLog, overlay::Canvas};
use crate::runtime::{
    self,
    ports::{Capturing, Footage, Pinning, SessionId, ShowEvent, Unchosen},
};

/// Frames larger than this on the long edge are halved; a reading model gains nothing from more.
pub(super) const LONGEST_EDGE: u32 = speakeasy_show::MAX_FRAME_EDGE;
/// The most compressed bytes a Show's journal holds; see [`speakeasy_show::Journal::with_budget`].
const JOURNAL_BUDGET: usize = speakeasy_show::Log::MAX_BUDGET;
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
const CAPTURE_STOPPED: &str = "Screen capture stopped unexpectedly. Start the Show again.";
/// The failure a Show reports when not one frame of the screen was recorded.
pub(super) const NOTHING_RECORDED: &str = "The Show recorded no frames. Start the Show again.";

/// The recorder unwound before reporting; its checkpoint may still contain usable frames.
#[derive(Debug)]
pub(crate) struct Unwound;

impl std::fmt::Display for Unwound {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(CAPTURE_STOPPED)
    }
}

impl std::error::Error for Unwound {}

/// A Show's screen recording; the session owner finishes or retires it like a microphone. Fields
/// drop in declaration order, so a recorder dropped without retiring cancels before its thread
/// joins.
pub(crate) struct ScreenRecorder {
    /// Sending finishes the recording; closing it, as dropping this only sender does, cancels.
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
        log: Option<FrameLog>,
        events: Sender<ShowEvent>,
        canvas: watch::Sender<Canvas>,
        shortcut: speakeasy_platform::MacShortcut,
    ) -> anyhow::Result<Self> {
        let (finish, finishing) = async_channel::bounded(1);
        let (pins, pinned) = async_channel::bounded(PIN_LANE);
        let thread = OwnedThread::spawn("show-recorder", move || {
            let lanes = Lanes {
                finishing,
                pinned,
                canvas: &canvas,
            };
            let mut report = Report::new(id, events.clone(), &lanes);
            report.outcome = record(started, scope, &lanes, id, events, log, shortcut);
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
        let Self { finish, thread, .. } = self;
        finish.close();
        thread.join()
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

/// How a recording ended.
enum Ended {
    Recorded(Box<Footage>),
    Unchosen(Unchosen),
    Cancelled,
}

/// Reports a recording however its thread leaves it: when dropped, it hides the overlay and tells
/// the owner how recording ended, with its footage, its failure, or that its scope found nothing to
/// record. A recording that finds itself cancelled reports nothing, even one that ended first; a
/// cancel that lands while it reports leaves the owner a report it ignores, as it does any from a
/// session it has ended.
struct Report<'a> {
    id: SessionId,
    events: Sender<ShowEvent>,
    lanes: &'a Lanes<'a>,
    /// How recording ended: [`Unwound`] until it returns, so a recording that unwinds
    /// reports a fixed failure rather than a panic's message, which could carry private text.
    outcome: anyhow::Result<Ended>,
}

impl<'a> Report<'a> {
    fn new(id: SessionId, events: Sender<ShowEvent>, lanes: &'a Lanes<'a>) -> Self {
        Self {
            id,
            events,
            lanes,
            outcome: Err(anyhow!(Unwound)),
        }
    }
}

impl Drop for Report<'_> {
    fn drop(&mut self) {
        self.lanes.canvas.send_replace(Canvas::Hidden);
        let event = match mem::replace(&mut self.outcome, Ok(Ended::Cancelled)) {
            Ok(Ended::Recorded(footage)) => ShowEvent::Finished(self.id, Ok(footage)),
            Ok(Ended::Unchosen(unchosen)) => ShowEvent::Unchosen(self.id, unchosen),
            Ok(Ended::Cancelled) => return,
            Err(error) => ShowEvent::Finished(self.id, Err(error)),
        };
        // A recording can end without seeing the cancel, as one looking for a window does, but the
        // owner that cancelled it waits for no report.
        if self.lanes.finishing.is_closed() {
            return;
        }
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A departed session owns checkpoint cleanup; this report contains only in-memory footage"
        )]
        let _ = self.events.send_blocking(event);
    }
}

/// What reaches the recording thread from the owner, and where it publishes to the overlay.
struct Lanes<'a> {
    /// A message finishes the recording; closing cancels it.
    finishing: Receiver<()>,
    pinned: Receiver<Taps>,
    canvas: &'a watch::Sender<Canvas>,
}

/// Records until finished or cancelled, returning how recording ended.
fn record(
    started: Instant,
    scope: Scope,
    lanes: &Lanes<'_>,
    id: SessionId,
    events: Sender<ShowEvent>,
    mut log: Option<FrameLog>,
    shortcut: speakeasy_platform::MacShortcut,
) -> anyhow::Result<Ended> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?
        .block_on(async {
            let target = match choose(scope, lanes).await? {
                Choice::Chosen(target) => target,
                Choice::Unchosen(unchosen) => return Ok(Ended::Unchosen(unchosen)),
                Choice::Cancelled => return Ok(Ended::Cancelled),
            };
            let (frame_lane, frames) = async_channel::bounded(FRAME_LANE);
            let capture =
                ScreenCapture::start(target, SAMPLE_INTERVAL, LONGEST_EDGE, frame_lane, None)?;
            let display = target.display();
            let (sample_lane, samples) = async_channel::bounded(POINTER_LANE);
            let pointer = PointerMonitor::start(display, sample_lane, shortcut)?;
            let part = target.part();
            lanes.canvas.send_replace(Canvas::covering(display, part));
            let sources = Sources {
                frames,
                samples,
                stop: stop(&capture, &pointer),
            };
            let mut take = Take::new(started, display.size, part, lanes.canvas);
            take.notices = Some((id, events));
            if let Some(writer) = &mut log
                && !writer.begin(JOURNAL_BUDGET, surface(display.size, part))
            {
                take.warn_checkpoint();
            }
            take.log = log;
            let footage = take.run(lanes, sources).await?;
            Ok(footage.map_or(Ended::Cancelled, |footage| {
                Ended::Recorded(Box::new(footage))
            }))
        })
}

/// How choosing what a Show's scope names ended.
enum Choice {
    Chosen(Target),
    Unchosen(Unchosen),
    Cancelled,
}

/// Resolves what `scope` names into a target. W takes the window under the pointer, and A the area
/// dragged out on the overlay over the display under the pointer.
async fn choose(scope: Scope, lanes: &Lanes<'_>) -> anyhow::Result<Choice> {
    let display = match scope {
        Scope::Screen => return Ok(Choice::Chosen(Target::screen_under_pointer()?)),
        Scope::Window => {
            return Ok(Target::window_under_pointer()?
                .map_or(Choice::Unchosen(Unchosen::Window), Choice::Chosen));
        },
        Scope::Area => Display::under_pointer()?,
    };
    let (chosen, choice) = async_channel::bounded(1);
    lanes
        .canvas
        .send_replace(Canvas::Picking { display, chosen });
    Ok(match pick(&choice, lanes).await {
        Picked::Area(part) => Choice::Chosen(Target::area(display, part)),
        Picked::Unchosen => Choice::Unchosen(Unchosen::Area),
        Picked::Cancelled => Choice::Cancelled,
    })
}

/// How picking an area ended.
#[derive(Debug, PartialEq)]
enum Picked {
    /// The part of the display dragged out.
    Area(Part),
    /// The Show finished first, a click chose no area, or the overlay could not appear to take a
    /// drag.
    Unchosen,
    Cancelled,
}

/// Waits for the part of the display dragged out on `choice`, which the overlay closes for a click
/// or when it cannot appear. Pins asked for before it returns mark nothing.
async fn pick(choice: &Receiver<Part>, lanes: &Lanes<'_>) -> Picked {
    let picked = tokio::select! {
        biased;
        finished = lanes.finishing.recv() => match finished {
            Ok(()) => Picked::Unchosen,
            Err(_closed) => Picked::Cancelled,
        },
        part = choice.recv() => part.map_or(Picked::Unchosen, Picked::Area),
    };
    while lanes.pinned.try_recv().is_ok() {}
    picked
}

/// Where a take's frames and pointer samples come from.
struct Sources<S> {
    frames: Receiver<ScreenEvent>,
    samples: Receiver<PointerSample>,
    /// Stops the capture and the pointer monitor once awaited, resolving when both have released
    /// the display and pointer.
    stop: S,
}

/// The thread's state while recording.
struct Take<'a> {
    started: Instant,
    canvas: &'a watch::Sender<Canvas>,
    /// The display's size in logical pixels.
    display: [f32; 2],
    /// The part of the display the latest frame showed, which the pointer is placed in.
    part: Part,
    state: speakeasy_show::Take,
    log: Option<FrameLog>,
    notices: Option<(SessionId, Sender<ShowEvent>)>,
    checkpoint_warned: bool,
}

impl<'a> Take<'a> {
    /// A take of the `part` of a display `size` logical pixels across, on a timeline that began
    /// at `started`, publishing ink and marks to `canvas`.
    fn new(
        started: Instant,
        size: [f32; 2],
        part: Part,
        canvas: &'a watch::Sender<Canvas>,
    ) -> Self {
        Self {
            started,
            canvas,
            display: size,
            part,
            state: speakeasy_show::Take::new(JOURNAL_BUDGET, surface(size, part)),
            log: None,
            notices: None,
            checkpoint_warned: false,
        }
    }

    /// Records what `sources` send until finished, then stops them before returning the footage;
    /// `None` once cancelled. A Show finished before its first frame waits up to
    /// [`FIRST_FRAME_WAIT`] for one. The footage lasts until finishing was asked for, the first
    /// frame such a wait received, or capture stopped on its own; stopping the sources adds
    /// nothing.
    async fn run(
        mut self,
        lanes: &Lanes<'_>,
        sources: Sources<impl Future<Output = ()>>,
    ) -> anyhow::Result<Option<Footage>> {
        let Sources {
            frames,
            samples,
            stop,
        } = sources;
        // Set once finished: the latest moment to keep waiting for a first frame.
        let mut finished_by = None;
        // When and how the take ended, or `None` once the owner retired this recording, which
        // reports nothing.
        let ended = loop {
            tokio::select! {
                biased;
                finished = lanes.finishing.recv() => {
                    if finished.is_err() {
                        break None;
                    }
                    if !self.state.journal().is_empty() {
                        break Some(Ok((runtime::now(), Ending::Finished)));
                    }
                    finished_by.get_or_insert_with(first_frame_deadline);
                },
                () = tokio::time::sleep_until(finished_by.unwrap_or_else(tokio::time::Instant::now)),
                    if finished_by.is_some() => break Some(Ok((runtime::now(), Ending::Finished))),
                event = frames.recv() => {
                    let (ending, failure) = match event {
                        Ok(ScreenEvent::Frame(frame)) => {
                            if let Err(error) = self.frame(frame) {
                                break Some(Err(error));
                            }
                            if finished_by.is_some() && !self.state.journal().is_empty() {
                                break Some(Ok((runtime::now(), Ending::Finished)));
                            }
                            continue;
                        },
                        Ok(ScreenEvent::WindowGone(gone)) => {
                            (window_ending(gone), anyhow!("{gone} Start the Show again."))
                        },
                        Ok(ScreenEvent::Failed(message)) => {
                            (Ending::ScreenStopped, anyhow!(message))
                        },
                        Ok(ScreenEvent::MovieStopped) => {
                            if let Some((id, events)) = &self.notices {
                                #[expect(clippy::let_underscore_must_use, reason = "A departed session no longer observes this capture's status")]
                                let _ = events.try_send(ShowEvent::MovieStopped(*id));
                            }
                            continue;
                        },
                        Err(_) => (Ending::ScreenStopped, anyhow!(CAPTURE_STOPPED)),
                    };
                    // Frames already journaled stay sound, so capture stopping partway ends the
                    // take with them. The footage notes only why: a failure's native text never
                    // reaches a saved file.
                    break Some(if self.state.journal().is_empty() {
                        Err(failure)
                    } else {
                        Ok((runtime::now(), ending))
                    });
                },
                // Samples first, so a pin lands where the pointer is and sees the taps that asked
                // for it.
                Ok(sample) = samples.recv() => {
                    if let Err(error) = self.pointer(sample) {
                        break Some(Err(error));
                    }
                },
                Ok(taps) = lanes.pinned.recv() => {
                    if let Err(error) = self.pin(taps) {
                        break Some(Err(error));
                    }
                },
            }
        };
        stop.await;
        match ended {
            Some(Ok((at, ending))) => self.into_footage(at, ending).await.map(Some),
            Some(Err(_invalid_event)) if !self.state.journal().is_empty() => {
                if let Some(log) = self.log.take() {
                    let _complete = log.close().await;
                }
                self.warn_checkpoint();
                self.into_footage(runtime::now(), Ending::ScreenStopped)
                    .await
                    .map(Some)
            },
            other => {
                if let Some(log) = self.log.take() {
                    let _complete = log.close().await;
                }
                other.transpose().map(|_| None)
            },
        }
    }

    fn warn_checkpoint(&mut self) {
        if self.checkpoint_warned {
            return;
        }
        self.checkpoint_warned = true;
        if let Some((id, events)) = &self.notices {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A departed owner cannot receive notices; a full lane must not stall screen capture"
            )]
            let _ = events.try_send(ShowEvent::CheckpointLost(*id));
        }
    }

    fn log(&mut self, event: TakeEvent) {
        if let Some(log) = &mut self.log
            && !log.send(event)
        {
            self.warn_checkpoint();
        }
    }

    fn apply(&mut self, event: TakeEvent) -> anyhow::Result<()> {
        self.state.apply(&event)?;
        self.log(event);
        Ok(())
    }

    fn moment(&self, at: Instant) -> Moment {
        Moment::from(at.saturating_duration_since(self.started))
    }

    fn frame(&mut self, frame: Frame) -> anyhow::Result<()> {
        if frame.part != self.part {
            self.part = frame.part;
            self.apply(TakeEvent::Surface(surface(self.display, frame.part)))?;
            self.canvas.send_if_modified(|canvas| {
                canvas
                    .sheet_mut()
                    .is_some_and(|sheet| sheet.track(frame.part))
            });
        }
        let at = self.moment(frame.at);
        let event = self.state.arrive(at, || {
            Image::from_bgra(frame.width, frame.height, frame.bgra).ok()
        })?;
        self.log(event);
        Ok(())
    }

    fn pointer(&mut self, sample: PointerSample) -> anyhow::Result<()> {
        let at = self.moment(sample.at);
        let earlier = self.state.tracker().strokes().len();
        let point = Point::from(self.part.within(sample.position));
        self.apply(TakeEvent::Pointer {
            at,
            point,
            drawing: sample.drawing,
            pressed: sample.pressed,
        })?;
        let finished: Vec<Arc<[Point]>> = self
            .state
            .tracker()
            .strokes()
            .iter()
            .skip(earlier)
            .map(|stroke| stroke.path().collect())
            .collect();
        let marks = (!finished.is_empty()).then(|| self.state.tracker().marks().into());
        self.publish_ink(finished, marks);
        Ok(())
    }

    fn pin(&mut self, taps: Taps) -> anyhow::Result<()> {
        let taps = Span::new(self.moment(taps.pressed), self.moment(taps.released));
        let before = self.state.tracker().marks().len();
        self.apply(TakeEvent::Pin(taps))?;
        if self.state.tracker().marks().len() != before {
            self.publish_ink(Vec::new(), Some(self.state.tracker().marks().into()));
        }
        Ok(())
    }

    /// Publishes the stroke being drawn to the overlay, with the strokes just `finished` and the
    /// `marks` when they changed.
    fn publish_ink(&self, finished: Vec<Arc<[Point]>>, marks: Option<Arc<[Mark]>>) {
        let drawing: Vec<Point> = self
            .state
            .tracker()
            .drawing()
            .iter()
            .map(|ink| ink.point)
            .collect();
        let now = Instant::now();
        self.canvas.send_if_modified(|canvas| {
            canvas
                .sheet_mut()
                .is_some_and(|sheet| sheet.update(finished, drawing, marks, now))
        });
    }

    /// What the take recorded, lasting until `ended`, and how it ended.
    async fn into_footage(mut self, ended: Instant, ending: Ending) -> anyhow::Result<Footage> {
        let ended = self.moment(ended);
        let event = self.state.end(ended, ending);
        self.log(event);
        if let Some(log) = self.log.take()
            && !log.close().await
        {
            self.warn_checkpoint();
        }
        if self.state.journal().is_empty() {
            return Err(anyhow!(NOTHING_RECORDED));
        }
        let (recording, journal) = self.state.finish();
        Ok(Footage { recording, journal })
    }
}

/// How a take ends once the window it records can no longer be followed.
const fn window_ending(gone: WindowGone) -> Ending {
    match gone {
        WindowGone::Closed => Ending::WindowClosed,
        WindowGone::OffScreen => Ending::WindowOffScreen,
    }
}

/// The size in logical pixels of the `part` of a display `display` logical pixels across.
fn surface([width, height]: [f32; 2], part: Part) -> Size {
    Size {
        width: width * part.width(),
        height: height * part.height(),
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
mod tests;

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
        let recorder = ScreenRecorder::start(
            SessionId::FIRST,
            started,
            Scope::Screen,
            None,
            events,
            canvas,
            speakeasy_platform::MacShortcut::default(),
        )?;
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        let display = ink
            .borrow()
            .display()
            .context("The overlay was given a display")?;
        recorder.finish();
        let ShowEvent::Finished(id, footage) =
            timeout(Duration::from_secs(10), finished.recv()).await??
        else {
            anyhow::bail!("The whole display was recorded, so something was chosen");
        };
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
            moments.is_sorted_by(|earlier, later| earlier < later),
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
            None,
            events,
            canvas,
            speakeasy_platform::MacShortcut::default(),
        )?;
        tokio::time::sleep(Duration::from_secs(10)).await;
        recorder.finish();
        let ShowEvent::Finished(_, footage) =
            timeout(Duration::from_secs(10), finished.recv()).await??
        else {
            anyhow::bail!("The whole display was recorded, so something was chosen");
        };
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
