use anyhow::ensure;
use async_channel::Sender;
use gpui::AsyncApp;

use super::*;

/// Clears the 30 fps that GPUI allows windows that are not active, which the pill never is, on a
/// 60 Hz display.
const MINIMUM_FRAMES_PER_SECOND: usize = 45;

#[test]
#[ignore = "Opt-in native pill window, never microphone, input, or clipboard"]
fn native_pill_animates_at_the_display_rate_after_idling() -> anyhow::Result<()> {
    run_native(check_frame_rate)
}

/// Runs `check` in a fresh GPUI application, one per test process.
pub(super) fn run_native(
    check: impl AsyncFnOnce(&AsyncApp) -> anyhow::Result<()> + 'static,
) -> anyhow::Result<()> {
    let (completed, outcome) = std::sync::mpsc::channel();
    let (application, presentation) = crate::application::new()?;
    application.run(move |cx| {
        cx.set_global(presentation);
        cx.spawn(async move |cx| {
            completed.send(check(cx).await).unwrap();
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    outcome
        .try_recv()
        .context("Native check did not complete")?
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance creates and observes GPUI windows on their owning UI thread"
)]
async fn check_frame_rate(cx: &AsyncApp) -> anyhow::Result<()> {
    let (publisher, updates) = watch::channel(Snapshot::default());
    let (_meter, levels) = watch::channel(0.0_f32);
    let pill = cx.update(|cx| {
        open(
            updates,
            levels,
            Keyboard::QUIET,
            false,
            Theme::default(),
            Opening::Startup,
            cx,
        )
    })?;
    // Windows stops vsync while no window is shown; showing the pill must start it again.
    cx.background_executor()
        .timer(Duration::from_millis(500))
        .await;
    publisher.send(Snapshot {
        phase: Phase::Processing,
        ..Snapshot::default()
    })?;
    cx.background_executor()
        .timer(Duration::from_millis(300))
        .await;
    let (counted, count) = async_channel::unbounded();
    cx.update(|cx| pill.update(cx, |_, window, _| count_frames(window, counted)))?;
    cx.background_executor().timer(Duration::from_secs(1)).await;
    count.close();
    let frames = count.len();
    ensure!(
        frames >= MINIMUM_FRAMES_PER_SECOND,
        "The processing pill drew {frames} frames in one second"
    );
    Ok(())
}

/// Counts every frame until the receiver closes.
fn count_frames(window: &Window, counted: Sender<()>) {
    window.on_next_frame(move |window, _| {
        if counted.try_send(()).is_ok() {
            count_frames(window, counted);
        }
    });
}
