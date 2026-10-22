//! The `--demo` preview: a scripted session published on the snapshot and meter channels. The
//! script never reaches the runtime, a microphone, or text insertion; its Show draws on the overlay
//! and records nothing.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::Timer;
use speakeasy_core::gesture::RECORDING_LIMIT;
use speakeasy_platform::Display;
use speakeasy_show::Point;
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
    let mut snapshot = starting(output.borrow().id.wrapping_add(1));
    present(&output, &snapshot, Duration::from_millis(350)).await;
    snapshot.phase = Phase::Recording;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        let seconds = start.elapsed().as_secs_f32();
        snapshot.hands_free = seconds > 2.0;
        let level =
            (seconds * 7.0).sin().mul_add(0.45, 0.35).max(0.0) * (seconds * 2.1).sin().abs();
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
    let show = snapshot.id.wrapping_add(1);
    play_show(&output, &meter, &canvas, show).await;
    play_interruptions(&output, show).await;
}

/// A held recording that S turns into a Show, circled on the overlay, finished hands-free, and
/// copied.
async fn play_show(
    output: &watch::Sender<Snapshot>,
    meter: &watch::Sender<Meter>,
    canvas: &watch::Sender<Canvas>,
    id: u64,
) {
    // A platform that records no Shows has no display to draw on, and the preview draws nothing.
    let display = Display::under_pointer().ok();
    let clear = Clear::new(canvas);
    let mut snapshot = starting(id);
    present(output, &snapshot, Duration::from_millis(250)).await;
    snapshot.phase = Phase::Recording;
    let start = Instant::now();
    let mut lifted = false;
    // Long enough for the drawing hint to hand back to the clock.
    while start.elapsed() < Duration::from_millis(4500) {
        let seconds = start.elapsed().as_secs_f32();
        let show = seconds > 0.8;
        if let Some(display) = display.filter(|_| show) {
            if !snapshot.show {
                canvas.send_replace(Canvas::covering(display));
            }
            if !lifted {
                lifted = circle(canvas, display, seconds);
            }
        }
        snapshot.show = show;
        snapshot.hands_free = show;
        let level = (seconds * 5.0).sin().mul_add(0.3, 0.3).max(0.0);
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
/// closes so the ink holds and fades as it does in a real Show. Returns whether the pen lifted.
fn circle(canvas: &watch::Sender<Canvas>, display: Display, seconds: f32) -> bool {
    const DRAWN_FROM: f32 = 1.0;
    const LIFTED_AT: f32 = 1.8;
    const RADIUS: f32 = 0.1;
    let progress = (seconds - DRAWN_FROM) / (LIFTED_AT - DRAWN_FROM);
    let [width, height] = display.size;
    // As densely as the pointer is sampled.
    let points: Vec<Point> = (0..=100_u16)
        .map(|step| f32::from(step) / 100.0)
        .take_while(|share| *share <= progress)
        .map(|share| {
            // A little past a full turn, as a hand closes a circle.
            let turn = share * std::f32::consts::TAU * 1.08;
            Point::new(
                (RADIUS * height / width).mul_add(turn.cos(), 0.5),
                RADIUS.mul_add(turn.sin(), 0.45),
            )
        })
        .collect();
    let (lifted, now) = (progress >= 1.0, Instant::now());
    canvas.send_if_modified(|canvas| {
        if lifted {
            canvas.update([Arc::from(points)], [], now)
        } else {
            canvas.update([], points, now)
        }
    });
    lifted
}

/// Each recording cuts off the previous ending, and a cancel is followed at once by the next
/// recording.
async fn play_interruptions(output: &watch::Sender<Snapshot>, mut id: u64) {
    for ending in [Phase::Cancelled, Phase::Empty, Phase::Copied, Phase::Error] {
        id = id.wrapping_add(1);
        let mut snapshot = starting(id);
        present(output, &snapshot, Duration::from_millis(180)).await;
        snapshot.phase = Phase::Recording;
        present(output, &snapshot, Duration::from_millis(700)).await;
        snapshot.phase = ending;
        snapshot.message = match ending {
            Phase::Copied => "Text copied. Release your shortcut keys and paste into your editor.",
            Phase::Error => {
                "Selected microphone is disconnected. Reconnect it or choose another in Settings."
            },
            _ => "",
        }
        .into();
        let held = if ending == Phase::Cancelled {
            Duration::from_millis(60)
        } else {
            Duration::from_secs(2)
        };
        present(output, &snapshot, held).await;
    }
}

fn starting(id: u64) -> Snapshot {
    Snapshot {
        id,
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
        session: snapshot.id,
        level,
    });
}
