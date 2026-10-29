//! Drives the area choice, a take on paused time with synthetic frames, pointer samples, and
//! pins, and the thread's ending, without a screen, a pointer, or an overlay window.

use anyhow::Context as _;
use speakeasy_show::{Action, Pin};

use super::*;

/// How long the fake capture and pointer monitor take to stop.
const TEARDOWN: Duration = Duration::from_millis(400);

/// What a test feeds a take, as the owner, the screen capture, and the pointer monitor would.
struct Feed {
    finish: Sender<()>,
    pins: Sender<Taps>,
    frames: Sender<ScreenEvent>,
    samples: Sender<PointerSample>,
    /// When the take's timeline began, on tokio's paused clock.
    started: Instant,
}

impl Feed {
    fn at(&self, millis: u64) -> Instant {
        self.started
            .checked_add(Duration::from_millis(millis))
            .unwrap()
    }

    /// A small frame of the whole display, on screen `millis` into the take.
    async fn frame(&self, millis: u64) {
        self.frame_showing(millis, Part::WHOLE).await;
    }

    /// A small frame of the `part` of the display, on screen `millis` into the take.
    async fn frame_showing(&self, millis: u64, part: Part) {
        let frame = Frame {
            at: self.at(millis),
            width: 4,
            height: 4,
            bgra: vec![0x80; 64],
            part,
        };
        self.frames.send(ScreenEvent::Frame(frame)).await.unwrap();
    }

    /// The pointer at `x`, `y` on the display `millis` into the take, drawing when `drawing` and
    /// with the button down when `pressed`.
    async fn pointer(&self, millis: u64, x: f32, y: f32, drawing: bool, pressed: bool) {
        let sample = PointerSample {
            at: self.at(millis),
            position: [x, y],
            drawing,
            pressed,
        };
        self.samples.send(sample).await.unwrap();
    }

    /// Two taps of Shift from `pressed` to `released` milliseconds into the take.
    async fn taps(&self, pressed: u64, released: u64) {
        let taps = Taps {
            pressed: self.at(pressed),
            released: self.at(released),
        };
        self.pins.send(taps).await.unwrap();
    }

    async fn finish(&self) {
        self.finish.send(()).await.unwrap();
    }
}

/// Records all of a 1000 × 500 display, finished from the start when `finished`, as `drive`
/// feeds it from sources that take [`TEARDOWN`] to stop. Returns what the take returned and how
/// long it ran on tokio's paused clock.
async fn record(
    finished: bool,
    drive: impl AsyncFnOnce(&Feed),
) -> (anyhow::Result<Option<Footage>>, Duration) {
    let (finish, finishing) = async_channel::bounded(1);
    let (pins, pinned) = async_channel::bounded(PIN_LANE);
    let (frame_lane, frames) = async_channel::bounded(FRAME_LANE);
    let (sample_lane, samples) = async_channel::bounded(POINTER_LANE);
    let (canvas, _) = watch::channel(Canvas::default());
    let lanes = Lanes {
        finishing,
        pinned,
        canvas: &canvas,
    };
    let start = tokio::time::Instant::now();
    let feed = Feed {
        finish,
        pins,
        frames: frame_lane,
        samples: sample_lane,
        started: start.into_std(),
    };
    let sources = Sources {
        frames,
        samples,
        stop: async { tokio::time::sleep(TEARDOWN).await },
    };
    let take = Take::new(start.into_std(), [1_000.0, 500.0], Part::WHOLE, &canvas);
    let run = async {
        let recorded = take.run(finished, &lanes, sources).await;
        (recorded, start.elapsed())
    };
    tokio::join!(run, drive(&feed)).0
}

/// What a recording that failed says.
fn failure<T>(recorded: anyhow::Result<T>) -> Option<String> {
    recorded.err().map(|error| error.to_string())
}

async fn wait(millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

#[tokio::test(start_paused = true)]
async fn a_take_finished_after_its_first_frame_ends_then_and_records_no_teardown()
-> anyhow::Result<()> {
    let (recorded, ran) = record(false, async |feed| {
        feed.frame(0).await;
        feed.pointer(200, 0.2, 0.2, false, true).await;
        feed.pointer(300, 0.6, 0.6, false, true).await;
        wait(1_000).await;
        feed.finish().await;
    })
    .await;
    let footage = recorded?.context("Finished, not cancelled")?;
    assert_eq!(footage.journal.samples().len(), 1);
    assert_eq!(ran, Duration::from_secs(1) + TEARDOWN, "Sources stop first");
    let finished = Moment::from_millis(1_000);
    assert_eq!(footage.recording.duration, finished);
    assert_eq!(
        footage.recording.actions,
        [Action::Drag {
            span: Span::new(Moment::from_millis(200), finished),
            from: Point::new(0.2, 0.2),
            to: Point::new(0.6, 0.6),
        }],
        "A drag still held when the Show finishes ends with it"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_take_finished_before_its_first_frame_waits_three_seconds_for_one() -> anyhow::Result<()>
{
    // Finished from the start, as an area Show finished while its area was being chosen.
    let (recorded, ran) = record(true, async |feed| {
        wait(2_900).await;
        feed.frame(2_900).await;
    })
    .await;
    let footage = recorded?.context("Finished, not cancelled")?;
    assert_eq!(footage.journal.samples().len(), 1);
    assert_eq!(footage.recording.duration, Moment::from_millis(2_900));
    assert_eq!(ran, Duration::from_millis(2_900) + TEARDOWN);
    let (recorded, ran) = record(false, async |feed| feed.finish().await).await;
    assert_eq!(failure(recorded).as_deref(), Some(NOTHING_RECORDED));
    assert_eq!(
        ran,
        FIRST_FRAME_WAIT + TEARDOWN,
        "Without a frame, the Show fails once the wait is over"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_take_reports_nothing_once_its_sources_stop() -> anyhow::Result<()> {
    let (recorded, ran) = record(false, async |feed| {
        feed.frame(0).await;
        wait(500).await;
        feed.finish.close();
    })
    .await;
    assert!(recorded?.is_none());
    assert_eq!(ran, Duration::from_millis(500) + TEARDOWN);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_capture_that_fails_or_stops_keeps_its_frames_or_says_what_to_do() -> anyhow::Result<()> {
    const OFF: &str = "Screen recording is off for Speakeasy. Turn it on, then try again.";
    let failed = || ScreenEvent::Failed(OFF.to_owned());
    let (recorded, _) = record(false, async |feed| {
        feed.frames.send(failed()).await.unwrap();
    })
    .await;
    assert_eq!(failure(recorded).as_deref(), Some(OFF));
    let (recorded, _) = record(false, async |feed| {
        feed.frames.close();
    })
    .await;
    assert_eq!(failure(recorded).as_deref(), Some(CAPTURE_STOPPED));
    let (recorded, _) = record(false, async |feed| {
        feed.frame(0).await;
        wait(700).await;
        feed.frames.send(failed()).await.unwrap();
    })
    .await;
    let footage = recorded?.context("Footage recorded before the failure")?;
    assert_eq!(footage.journal.samples().len(), 1);
    assert_eq!(footage.recording.duration, Moment::from_millis(700));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_window_that_resizes_is_measured_at_its_latest_size() -> anyhow::Result<()> {
    let (recorded, _) = record(false, async |feed| {
        feed.frame(0).await;
        feed.frame_showing(200, Part::between([0.0, 0.0], [0.5, 1.0]))
            .await;
        wait(10).await;
        feed.finish().await;
    })
    .await;
    let recording = recorded?.context("Finished, not cancelled")?.recording;
    assert_eq!(
        recording.surface,
        Size {
            width: 500.0,
            height: 500.0
        }
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_pin_is_judged_after_the_pointer_samples_queued_before_it() -> anyhow::Result<()> {
    let (recorded, _) = record(false, async |feed| {
        feed.frame(0).await;
        feed.pointer(10, 0.5, 0.5, false, false).await;
        wait(10).await;
        feed.pointer(100, 0.5, 0.5, true, false).await;
        feed.pointer(150, 0.52, 0.5, true, false).await;
        feed.pointer(200, 0.52, 0.5, false, false).await;
        feed.taps(50, 250).await;
        wait(10).await;
        feed.pointer(300, 0.3, 0.4, false, false).await;
        feed.taps(400, 600).await;
        wait(10).await;
        feed.finish().await;
    })
    .await;
    let recording = recorded?.context("Finished, not cancelled")?.recording;
    assert_eq!(recording.strokes.len(), 1);
    assert_eq!(
        recording.pins,
        [Pin {
            at: Moment::from_millis(600),
            point: Point::new(0.3, 0.4),
        }],
        "taps queued after the ink they span ask for no pin; clean taps pin where the pointer \
         moved just before them"
    );
    Ok(())
}

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
    assert!(lanes.pinned.is_empty(), "The early pin was dropped");
    finish.try_send(())?;
    assert_eq!(pick(&choice, &lanes).await, Some((Part::WHOLE, true)));
    chosen.close();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), pick(&choice, &lanes)).await?,
        Some((Part::WHOLE, false)),
        "The overlay could not appear"
    );
    finish.close();
    assert_eq!(pick(&choice, &lanes).await, None, "Cancelled");
    Ok(())
}

#[test]
fn a_recording_that_unwinds_hides_the_overlay_and_reports_a_fixed_failure() -> anyhow::Result<()> {
    let (events, reported) = async_channel::bounded(1);
    let (canvas, overlay) = watch::channel(Canvas::default());
    let unwound = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _ending = Ending::new(SessionId::FIRST, events, &canvas);
                panic!("A recording bug");
            })
            .join()
    });
    assert!(unwound.is_err());
    assert!(overlay.has_changed()?, "The overlay was told to hide");
    let ShowEvent::Finished(id, outcome) = reported.try_recv()?;
    assert_eq!(id, SessionId::FIRST);
    assert_eq!(failure(outcome).as_deref(), Some(CAPTURE_STOPPED));
    Ok(())
}
