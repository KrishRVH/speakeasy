//! The `--demo` preview: a scripted session published on the snapshot channel. The script never
//! reaches the runtime, a microphone, or text insertion.

use std::time::{Duration, Instant};

use gpui::Timer;
use speakeasy_core::gesture::RECORDING_LIMIT;
use speakeasy_dictation::runtime::{ModelState, Phase, Snapshot};
use tokio::sync::watch;

use crate::pill::COUNTDOWN;

/// Two seconds into the pill's countdown, so the preview shows it.
const NEAR_RECORDING_LIMIT: Duration = RECORDING_LIMIT
    .saturating_sub(COUNTDOWN)
    .saturating_add(Duration::from_secs(2));

pub(super) async fn play(output: watch::Sender<Snapshot>) {
    let mut snapshot = starting(output.borrow().id.wrapping_add(1));
    show(&output, &snapshot, Duration::from_millis(350)).await;
    snapshot.phase = Phase::Recording;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        let seconds = start.elapsed().as_secs_f32();
        snapshot.hands_free = seconds > 2.0;
        snapshot.meter_tick = snapshot.meter_tick.wrapping_add(1);
        snapshot.level =
            (seconds * 7.0).sin().mul_add(0.45, 0.35).max(0.0) * (seconds * 2.1).sin().abs();
        show(&output, &snapshot, Duration::from_millis(32)).await;
    }
    snapshot.started = Instant::now()
        .checked_sub(NEAR_RECORDING_LIMIT)
        .unwrap_or_else(Instant::now);
    snapshot.level = 0.0;
    snapshot.meter_tick = snapshot.meter_tick.wrapping_add(1);
    show(&output, &snapshot, Duration::from_secs(2)).await;
    for (phase, duration) in [
        (Phase::Stopping, Duration::from_millis(150)),
        (Phase::Processing, Duration::from_millis(1400)),
        (Phase::Done, Duration::from_millis(450)),
    ] {
        snapshot.phase = phase;
        show(&output, &snapshot, duration).await;
    }
    play_interruptions(&output, snapshot.id).await;
}

/// Each recording cuts off the previous ending, and a cancel is followed at once by the next
/// recording.
async fn play_interruptions(output: &watch::Sender<Snapshot>, mut id: u64) {
    for ending in [Phase::Cancelled, Phase::Empty, Phase::Error] {
        id = id.wrapping_add(1);
        let mut snapshot = starting(id);
        show(output, &snapshot, Duration::from_millis(180)).await;
        snapshot.phase = Phase::Recording;
        show(output, &snapshot, Duration::from_millis(700)).await;
        snapshot.phase = ending;
        if ending == Phase::Error {
            snapshot.message =
                "Microphone disconnected. Choose an available microphone in Settings.".into();
        }
        let held = if ending == Phase::Cancelled {
            Duration::from_millis(60)
        } else {
            Duration::from_secs(2)
        };
        show(output, &snapshot, held).await;
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

async fn show(output: &watch::Sender<Snapshot>, snapshot: &Snapshot, duration: Duration) {
    output.send_replace(snapshot.clone());
    Timer::after(duration).await;
}
