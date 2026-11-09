use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use async_channel::{Receiver, Sender};
use speakeasy_platform::{
    Display, Frame, OwnedThread, Part, PointerMonitor, PointerSample, Scope, ScreenCapture,
    ScreenEvent, Taps, Target, WindowGone,
};
use speakeasy_show::{
    Ending, Framing, Image, MAX_FRAME_EDGE, Mark, Moment, Point, SAMPLE_INTERVAL, Size, Span,
    TakeEvent,
};
use tokio::sync::watch;

use super::{FrameLog, overlay::Canvas};
use crate::runtime::{
    self,
    ports::{Capturing, Footage, Pinning, SessionId, ShowEvent, Unchosen},
};

const JOURNAL_BUDGET: usize = speakeasy_show::Log::MAX_BUDGET;

const FIRST_FRAME_WAIT: Duration = Duration::from_secs(3);

const FRAME_LANE: usize = 2;

const POINTER_LANE: usize = 256;

const PIN_LANE: usize = 8;

const CAPTURE_STOPPED: &str = "Screen capture stopped unexpectedly. Start the Show again.";

pub(super) const NOTHING_RECORDED: &str = "The Show recorded no frames. Start the Show again.";

#[derive(Debug)]
pub(crate) struct Unwound;

impl std::fmt::Display for Unwound {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(CAPTURE_STOPPED)
    }
}

impl std::error::Error for Unwound {}

pub(crate) struct ScreenRecorder {
    finish: Sender<()>,
    pins: Sender<Taps>,
    thread: OwnedThread,
}

impl ScreenRecorder {
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
            let report = Report::new(id, events.clone(), &lanes);
            let outcome = record(started, scope, &lanes, id, events, log, shortcut);
            report.deliver(outcome);
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
        async move {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The recorder report carries the outcome; retirement waits for all native cleanup even after a panic"
            )]
            let _ = thread.join().await;
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

enum Ended {
    Recorded(Box<Footage>),
    Unchosen(Unchosen),
    Cancelled,
}

struct Report<'a> {
    id: SessionId,
    events: Sender<ShowEvent>,
    lanes: &'a Lanes<'a>,
    armed: bool,
}

impl<'a> Report<'a> {
    fn new(id: SessionId, events: Sender<ShowEvent>, lanes: &'a Lanes<'a>) -> Self {
        Self {
            id,
            events,
            lanes,
            armed: true,
        }
    }

    fn deliver(mut self, outcome: anyhow::Result<Ended>) {
        self.armed = false;
        self.send(outcome);
    }

    fn send(&self, outcome: anyhow::Result<Ended>) {
        self.lanes.canvas.send_replace(Canvas::Hidden);
        let event = match outcome {
            Ok(Ended::Recorded(footage)) => ShowEvent::Finished(self.id, Ok(footage)),
            Ok(Ended::Unchosen(unchosen)) => ShowEvent::Unchosen(self.id, unchosen),
            Ok(Ended::Cancelled) => return,
            Err(error) => ShowEvent::Finished(self.id, Err(error)),
        };

        if self.lanes.finishing.is_closed() {
            return;
        }
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The dedicated recorder thread retains its terminal outcome until the owner receives it; a closed lane means that owner has left"
        )]
        let _ = self.events.send_blocking(event);
    }
}

impl Drop for Report<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.send(Err(anyhow!(Unwound)));
        }
    }
}

struct Lanes<'a> {
    finishing: Receiver<()>,
    pinned: Receiver<Taps>,
    canvas: &'a watch::Sender<Canvas>,
}

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
            let display = target.display();
            let part = target.part();
            let framing = match scope {
                Scope::Screen => Framing::Automatic,
                Scope::Window | Scope::Area => Framing::Chosen,
            };
            lanes.canvas.send_replace(Canvas::covering(display, part));
            let mut take = Take::new(
                started,
                display.size,
                part,
                framing,
                lanes.canvas,
                (id, events),
            );
            if let Some(writer) = &mut log
                && !writer.begin(JOURNAL_BUDGET, surface(display.size, part), framing)
            {
                take.warn_checkpoint();
            }
            let movie = match &mut log {
                Some(writer) => writer.movie(started).await,
                None => None,
            };
            if lanes.finishing.is_closed() {
                return Ok(Ended::Cancelled);
            }
            let (frame_lane, frames) = async_channel::bounded(FRAME_LANE);
            let capture =
                ScreenCapture::start(target, SAMPLE_INTERVAL, MAX_FRAME_EDGE, frame_lane, movie)?;
            let (sample_lane, samples) = async_channel::bounded(POINTER_LANE);
            let pointer = PointerMonitor::start(target, sample_lane, shortcut)?;
            let sources = Sources {
                frames,
                samples,
                stop: stop(&capture, &pointer),
            };
            take.log = log;
            let footage = take.run(lanes, sources).await?;
            Ok(footage.map_or(Ended::Cancelled, |footage| {
                Ended::Recorded(Box::new(footage))
            }))
        })
}

enum Choice {
    Chosen(Target),
    Unchosen(Unchosen),
    Cancelled,
}

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

#[derive(Debug, PartialEq)]
enum Picked {
    Area(Part),
    Unchosen,
    Cancelled,
}

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

struct Sources<S> {
    frames: Receiver<ScreenEvent>,
    samples: Receiver<PointerSample>,
    stop: S,
}

struct Take<'a> {
    started: Instant,
    canvas: &'a watch::Sender<Canvas>,
    display: [f32; 2],
    part: Part,
    placed_at: Instant,
    state: speakeasy_show::Take,
    log: Option<FrameLog>,
    notices: (SessionId, Sender<ShowEvent>),
    checkpoint_warned: bool,
}

impl<'a> Take<'a> {
    fn new(
        started: Instant,
        size: [f32; 2],
        part: Part,
        framing: Framing,
        canvas: &'a watch::Sender<Canvas>,
        notices: (SessionId, Sender<ShowEvent>),
    ) -> Self {
        Self {
            started,
            canvas,
            display: size,
            part,
            placed_at: started,
            state: speakeasy_show::Take::new(JOURNAL_BUDGET, surface(size, part), framing),
            log: None,
            notices,
            checkpoint_warned: false,
        }
    }

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

        let mut finished_by = None;

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
                            let (id, events) = &self.notices;
                            #[expect(clippy::let_underscore_must_use, reason = "A departed session no longer observes this capture's status")]
                            let _ = events.try_send(ShowEvent::MovieStopped(*id));
                            continue;
                        },
                        Err(_) => (Ending::ScreenStopped, anyhow!(CAPTURE_STOPPED)),
                    };

                    break Some(if self.state.journal().is_empty() {
                        Err(failure)
                    } else {
                        Ok((runtime::now(), ending))
                    });
                },
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
        let ended = ended.map(|result| {
            result.and_then(|(at, ending)| {
                std::iter::from_fn(|| samples.try_recv().ok())
                    .filter(|sample| sample.at() <= at)
                    .try_for_each(|sample| self.pointer(sample))?;
                Ok((at, ending))
            })
        });
        let (at, ending) = match ended {
            Some(Ok(end)) => end,
            Some(Err(_invalid_event)) if !self.state.journal().is_empty() => {
                self.abandon_log().await;
                self.warn_checkpoint();
                (runtime::now(), Ending::ScreenStopped)
            },
            Some(Err(error)) => {
                self.abandon_log().await;
                return Err(error);
            },
            None => {
                self.abandon_log().await;
                return Ok(None);
            },
        };
        self.into_footage(at, ending).await.map(Some)
    }

    async fn abandon_log(&mut self) {
        if let Some(log) = self.log.take() {
            let _flushed = log.close().await;
        }
    }

    fn warn_checkpoint(&mut self) {
        if self.checkpoint_warned {
            return;
        }
        self.checkpoint_warned = true;
        let (id, events) = &self.notices;
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A departed owner cannot receive notices; a full lane must not stall screen capture"
        )]
        let _ = events.try_send(ShowEvent::CheckpointLost(*id));
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
        self.place(frame.part, frame.at)?;
        let at = self.moment(frame.at);
        let event = self.state.arrive(at, || {
            Image::from_bgra(frame.width, frame.height, frame.bgra).ok()
        })?;
        self.log(event);
        Ok(())
    }

    fn place(&mut self, part: Part, at: Instant) -> anyhow::Result<()> {
        if at < self.placed_at {
            return Ok(());
        }
        self.placed_at = at;
        if part != self.part {
            self.apply(TakeEvent::Surface(surface(self.display, part)))?;
            self.part = part;
            self.canvas.send_if_modified(|canvas| {
                canvas.sheet_mut().is_some_and(|sheet| sheet.track(part))
            });
        }
        Ok(())
    }

    fn pointer(&mut self, sample: PointerSample) -> anyhow::Result<()> {
        let at = self.moment(sample.at());
        let earlier = self.state.tracker().strokes().len();
        match sample {
            PointerSample::Gap(_) => self.apply(TakeEvent::PointerGap(at))?,
            PointerSample::Observed {
                at: sampled_at,
                position,
                part,
                drawing,
                pressed,
            } => {
                self.place(part, sampled_at)?;
                let local = surface(self.display, part);
                let latest = surface(self.display, self.part);
                if local != latest {
                    self.apply(TakeEvent::Surface(local))?;
                }
                self.apply(TakeEvent::Pointer {
                    at,
                    point: Point::from(part.within(position)),
                    part: [part.left, part.top, part.right, part.bottom].into(),
                    drawing,
                    pressed,
                })?;
                if local != latest {
                    self.apply(TakeEvent::Surface(latest))?;
                }
            },
        }
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
        let framing = self.state.framing();
        let (recording, journal) = self.state.finish();
        Ok(Footage {
            recording,
            journal,
            framing,
        })
    }
}

const fn window_ending(gone: WindowGone) -> Ending {
    match gone {
        WindowGone::Closed => Ending::WindowClosed,
        WindowGone::OffScreen => Ending::WindowOffScreen,
    }
}

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

async fn stop(capture: &ScreenCapture, pointer: &PointerMonitor) {
    capture.request_stop();
    pointer.request_stop();
    tokio::join!(capture.stopped(), pointer.stopped());
}

#[cfg(test)]
mod tests;

#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
mod native_tests {
    use anyhow::Context as _;
    use tokio::time::timeout;

    use super::*;

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
        assert!(frame.width().max(frame.height()) <= MAX_FRAME_EDGE);
        let (aspect, surface) = (frame.aspect(), width / height);
        assert!((aspect - surface).abs() < 0.02, "{aspect} vs {surface}");
        Ok(())
    }

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
