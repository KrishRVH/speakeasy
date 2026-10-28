//! The `--demo` preview: a scripted session published on the snapshot and meter channels. The
//! script never reaches the runtime, a microphone, or text insertion; its Show draws on the overlay
//! and records nothing.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::Timer;
use speakeasy_core::gesture::RECORDING_LIMIT;
use speakeasy_platform::{Display, Part};
use speakeasy_show::{Mark, Moment, Point, Rect, Shape, Span};
use tokio::sync::watch;

use crate::{
    pill::COUNTDOWN,
    runtime::{Meter, ModelState, Phase, Snapshot},
    show::Canvas,
};

/// Two seconds into the pill's countdown, so the preview shows it.
const NEAR_RECORDING_LIMIT: Duration = RECORDING_LIMIT
    .saturating_sub(COUNTDOWN)
    .saturating_add(Duration::from_secs(2));

pub(super) async fn play(
    output: watch::Sender<Snapshot>,
    meter: watch::Sender<Meter>,
    canvas: watch::Sender<Canvas>,
) {
    let mut snapshot = starting(output.borrow().session.wrapping_add(1));
    present(&output, &snapshot, Duration::from_millis(350)).await;
    snapshot.phase = Phase::Recording;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        let seconds = start.elapsed().as_secs_f32();
        snapshot.hands_free = seconds > 2.0;
        let level = (0.35 + 0.45 * (seconds * 7.0).sin()).max(0.0) * (seconds * 2.1).sin().abs();
        measure(&meter, &snapshot, level);
        present(&output, &snapshot, Duration::from_millis(32)).await;
    }
    snapshot.started = Instant::now()
        .checked_sub(NEAR_RECORDING_LIMIT)
        .unwrap_or_else(Instant::now);
    measure(&meter, &snapshot, 0.0);
    present(&output, &snapshot, Duration::from_secs(2)).await;
    for (phase, duration) in [
        (Phase::Stopping, Duration::from_millis(150)),
        (Phase::Processing, Duration::from_millis(1400)),
        (Phase::Done, Duration::from_millis(450)),
    ] {
        snapshot.phase = phase;
        present(&output, &snapshot, duration).await;
    }
    let show_session = snapshot.session.wrapping_add(1);
    play_show(&output, &meter, &canvas, show_session).await;
    play_interruptions(&output, show_session).await;
}

/// A held recording that S turns into a Show, circled on the overlay, finished hands-free, and
/// copied.
async fn play_show(
    output: &watch::Sender<Snapshot>,
    meter: &watch::Sender<Meter>,
    canvas: &watch::Sender<Canvas>,
    session: u64,
) {
    // A platform that records no Shows has no display to draw on, and the preview draws nothing.
    let display = Display::under_pointer().ok();
    let clear = Clear::new(canvas);
    let mut snapshot = starting(session);
    present(output, &snapshot, Duration::from_millis(250)).await;
    snapshot.phase = Phase::Recording;
    let start = Instant::now();
    let mut lifted = false;
    // Long enough for the drawing hint to hand back to the clock.
    while start.elapsed() < Duration::from_millis(4500) {
        let seconds = start.elapsed().as_secs_f32();
        let show = seconds > 0.8;
        if show && let Some(display) = display {
            if !snapshot.show {
                canvas.send_replace(Canvas::covering(display, Part::WHOLE));
            }
            if !lifted {
                lifted = circle(canvas, display, seconds);
            }
        }
        snapshot.show = show;
        snapshot.hands_free = show;
        let level = (0.3 + 0.3 * (seconds * 5.0).sin()).max(0.0);
        measure(meter, &snapshot, level);
        present(output, &snapshot, Duration::from_millis(32)).await;
    }
    drop(clear);
    snapshot.hands_free = false;
    for (phase, duration) in [
        (Phase::Stopping, Duration::from_millis(150)),
        (Phase::Processing, Duration::from_millis(1200)),
        (Phase::Copied, Duration::from_millis(1600)),
    ] {
        snapshot.phase = phase;
        present(output, &snapshot, duration).await;
    }
}

/// Empties the overlay's canvas when made and again however the Show ends, including when a replay
/// drops the preview mid-stroke.
struct Clear<'a>(&'a watch::Sender<Canvas>);

impl<'a> Clear<'a> {
    fn new(canvas: &'a watch::Sender<Canvas>) -> Self {
        canvas.send_replace(Canvas::default());
        Self(canvas)
    }
}

impl Drop for Clear<'_> {
    fn drop(&mut self) {
        self.0.send_replace(Canvas::default());
    }
}

/// Draws a circle on `display` from one second into the recording, lifting the pen as the circle
/// closes so the ink and its number hold and fade as they do in a real Show. Returns whether the
/// pen lifted.
fn circle(canvas: &watch::Sender<Canvas>, display: Display, seconds: f32) -> bool {
    const DRAWN_FROM: f32 = 1.0;
    const LIFTED_AT: f32 = 1.8;
    const CENTER: Point = Point::new(0.5, 0.45);
    const RADIUS: f32 = 0.1;
    // A little past one turn, as a hand closes a circle.
    const TURNS: f32 = 1.08;
    let progress = (seconds - DRAWN_FROM) / (LIFTED_AT - DRAWN_FROM);
    let [width, height] = display.size;
    let across = RADIUS * height / width;
    // As densely as the pointer is sampled.
    let points: Vec<Point> = (0..=100_u16)
        .map(|step| f32::from(step) / 100.0)
        .take_while(|&share| share <= progress)
        .map(|share| {
            let turn = share * std::f32::consts::TAU * TURNS;
            Point::new(
                CENTER.x + across * turn.cos(),
                CENTER.y + RADIUS * turn.sin(),
            )
        })
        .collect();
    let lifted = progress >= 1.0;
    let now = Instant::now();
    canvas.send_if_modified(|canvas| {
        let Some(sheet) = canvas.sheet_mut() else {
            return false;
        };
        if lifted {
            let mark = Mark {
                number: 1,
                shape: Shape::Circle,
                strokes: 0..1,
                span: Span::at(Moment::START),
                bounds: Rect {
                    left: CENTER.x - across,
                    top: CENTER.y - RADIUS,
                    right: CENTER.x + across,
                    bottom: CENTER.y + RADIUS,
                },
            };
            sheet.update([Arc::from(points)], [], Some(Arc::new([mark])), now)
        } else {
            sheet.update([], points, None, now)
        }
    });
    lifted
}

/// Each recording cuts off the previous ending, and a cancel is followed at once by the next
/// recording.
async fn play_interruptions(output: &watch::Sender<Snapshot>, mut session: u64) {
    for (ending, message, held) in [
        (Phase::Cancelled, "", Duration::from_millis(60)),
        (Phase::Empty, "", Duration::from_secs(2)),
        (
            Phase::Copied,
            "Text copied. Release your shortcut keys and paste into your editor.",
            Duration::from_secs(2),
        ),
        (
            Phase::Error,
            "The selected microphone is disconnected. Reconnect it, or choose another microphone in Settings.",
            Duration::from_secs(2),
        ),
    ] {
        session = session.wrapping_add(1);
        let mut snapshot = starting(session);
        present(output, &snapshot, Duration::from_millis(180)).await;
        snapshot.phase = Phase::Recording;
        present(output, &snapshot, Duration::from_millis(700)).await;
        snapshot.phase = ending;
        snapshot.message = message.into();
        present(output, &snapshot, held).await;
    }
}

fn starting(session: u64) -> Snapshot {
    Snapshot {
        session,
        phase: Phase::Starting,
        model: ModelState::Ready,
        ..Snapshot::default()
    }
}

/// Publishes `snapshot` unless it is already current, as the owner does, then holds it.
async fn present(output: &watch::Sender<Snapshot>, snapshot: &Snapshot, duration: Duration) {
    output.send_if_modified(|current| {
        let changed = *current != *snapshot;
        if changed {
            current.clone_from(snapshot);
        }
        changed
    });
    Timer::after(duration).await;
}

/// Reports `level` as the meter of `snapshot`'s session.
fn measure(meter: &watch::Sender<Meter>, snapshot: &Snapshot, level: f32) {
    meter.send_replace(Meter {
        epoch: snapshot.epoch,
        session: snapshot.session,
        level,
    });
}
