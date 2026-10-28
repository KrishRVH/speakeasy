//! Dictation scenarios: capture, gestures and deadlines, shutdown ordering, inference, insertion,
//! configuration, lifecycle epochs, warmup, failure reports, and the reply-to-insertion latency
//! profile.

use super::*;
use crate::{
    config::Engine,
    status::{PAUSED, PAUSING},
    theme::Theme,
};

#[tokio::test(start_paused = true)]
async fn desktop_readiness_blocks_capture_until_native_startup_completes() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| ports.desktop_pending = true)?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.input(Input::Toggle);
    h.input(Input::DesktopReady {
        shortcut: "fixture shortcut".into(),
        cancel: "fixture cancel".into(),
    });
    h.observe(|snapshot| snapshot.desktop_ready).await?;
    assert!(h.captures.try_recv().is_err());
    assert_eq!(&*h.snapshot().shortcut, "fixture shortcut");
    assert_eq!(&*h.snapshot().cancel_shortcut, "fixture cancel");
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn held_shortcut_release_finishes_through_the_owner() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    tokio::time::advance(Duration::from_millis(250)).await;
    h.input(Input::Release);
    h.transcribe_next_as("").await?;
    h.phase(Phase::Empty).await?;
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn locking_a_hold_keeps_the_same_capture_until_the_next_press() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Lock);
    h.observe(|snapshot| snapshot.hands_free).await?;
    h.input(Input::Release);
    events.send(CaptureEvent::Level(id, 0.25)).await?;
    h.observe_level(0.25).await?;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(h.jobs.try_recv().is_err());
    assert!(h.captures.try_recv().is_err());
    h.input(Input::Press);
    receive(&h.jobs).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn double_tap_uses_one_capture_and_single_tap_obeys_its_deadline() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    tokio::time::advance(Duration::from_millis(50)).await;
    h.input(Input::Release);
    events.send(CaptureEvent::Level(id, 0.25)).await?;
    h.observe_level(0.25).await?;
    tokio::time::advance(Duration::from_millis(100)).await;
    h.input(Input::Press);
    h.observe(|snapshot| snapshot.hands_free).await?;
    assert!(h.captures.try_recv().is_err());
    assert!(h.jobs.try_recv().is_err());
    h.input(Input::Release);
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;

    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    tokio::time::advance(Duration::from_millis(50)).await;
    h.input(Input::Release);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    events.send(CaptureEvent::Level(id, 0.5)).await?;
    h.observe_level(0.5).await?;
    tokio::time::advance(Duration::from_millis(299)).await;
    assert!(h.jobs.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    receive(&h.jobs).await?;
    assert_eq!(tokio::time::Instant::now(), deadline);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn owner_finishes_capture_at_the_five_minute_deadline() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    let limit = Duration::from_secs(300);
    let deadline = tokio::time::Instant::now() + limit;
    tokio::time::advance(limit.saturating_sub(Duration::from_millis(1))).await;
    assert!(h.jobs.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    receive(&h.jobs).await?;
    assert_eq!(tokio::time::Instant::now(), deadline);
    assert!(h.captures.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn failed_capture_start_preserves_its_reason_and_allows_shutdown() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.record_error = Some("thread resources exhausted");
    })?;
    h.input(Input::Press);
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("thread resources exhausted"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancelled_capture_retires_before_retry_and_pause_waits_for_it() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
    })?;
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    retiring.reached().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Starting).await?;
    assert!(
        still_pending(h.captures.recv()).await,
        "Retry overlapped a retiring microphone"
    );
    retiring.release().await?;
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.runtime.request_stop();
    retiring.reached().await?;
    assert!(still_pending(h.exited()).await);
    retiring.release().await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn pause_during_a_queued_capture_never_opens_the_new_device() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
    })?;
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    retiring.reached().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Starting).await?;
    h.runtime.request_stop();
    assert!(still_pending(h.exited()).await);
    retiring.release().await?;
    timeout(PATIENCE, h.exited()).await?;
    assert!(h.captures.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn completed_audio_does_not_allow_retry_before_device_teardown() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
    })?;
    h.start().await?;
    h.finish_as("fixture").await?;
    retiring.reached().await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    h.input(Input::Toggle);
    h.phase(Phase::Starting).await?;
    assert!(still_pending(h.captures.recv()).await);
    retiring.release().await?;
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.runtime.request_stop();
    retiring.reached().await?;
    retiring.release().await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn pause_starts_capture_and_model_cleanup_together() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(1);
    let (stop, stopping) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
        ports.stop = Some(stop);
    })?;
    receive(&h.loads).await?;
    h.start().await?;
    h.runtime.request_stop();
    retiring.reached().await?;
    stopping.reached().await?;
    assert!(still_pending(h.exited()).await);
    retiring.release().await?;
    stopping.release().await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cpu_cancellation_reaps_before_replacement_without_blocking_input() -> anyhow::Result<()> {
    let (stop, stopping) = gate(2);
    let config = Config {
        use_gpu: false,
        ..Config::default()
    };
    let mut h = Harness::with_ports(config, |ports| ports.stop = Some(stop))?;
    receive(&h.loads).await?;
    h.start().await?;
    let mut old_reply = h.finish().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    stopping.reached().await?;
    timeout(PATIENCE, old_reply.closed()).await?;
    assert!(answer(old_reply, "stale fixture").is_err());
    assert!(
        still_pending(h.loads.recv()).await,
        "Replacement started before old worker exit"
    );
    h.start().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Processing).await?;
    assert!(still_pending(h.jobs.recv()).await);
    stopping.release().await?;
    receive(&h.loads).await?;
    h.transcribe_next_as("current fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "current fixture");
    h.phase(Phase::Done).await?;
    h.runtime.request_stop();
    stopping.reached().await?;
    stopping.release().await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn preparing_insertion_cancels_without_blocking_a_new_recording() -> anyhow::Result<()> {
    let (insertion, preparing) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.insertion = Some(insertion);
    })?;
    h.start().await?;
    h.finish_as("old fixture").await?;
    let old = preparing.reached().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.start().await?;
    assert!(!old.active(), "A newer capture authorized stale insertion");
    preparing.release().await?;
    h.finish_as("new fixture").await?;
    let next = preparing.reached().await?;
    assert!(next.active());
    assert!(!old.commit());
    preparing.release().await?;
    assert_eq!(receive(&h.pasted).await?, "new fixture");
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn shutdown_revokes_and_owns_pending_insertion_cleanup() -> anyhow::Result<()> {
    let (insertion, preparing) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.insertion = Some(insertion);
    })?;
    h.start().await?;
    h.finish_as("fixture").await?;
    let permit = preparing.reached().await?;
    h.runtime.request_stop();
    assert!(!permit.active());
    assert!(still_pending(h.exited()).await);
    preparing.release().await?;
    timeout(PATIENCE, h.exited()).await?;
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn silence_and_empty_recognition_never_report_submission() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, events) = h.start().await?;
    events.send(CaptureEvent::Finished(id, Ok(None))).await?;
    h.phase(Phase::Empty).await?;
    assert!(h.jobs.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    for text in ["", "\t\r\n", "Um, uh. UM!"] {
        h.start().await?;
        h.finish_as(text).await?;
        h.phase(Phase::Empty).await?;
        assert!(h.pasted.try_recv().is_err());
    }
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn copied_text_is_a_notice_that_carries_its_remedy_until_the_next_paste() -> anyhow::Result<()>
{
    const REMEDY: &str = "Text copied. Release your shortcut keys and paste into your editor.";
    let (results, queued) = async_channel::bounded(1);
    results.try_send(Inserted::Copied(REMEDY))?;
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.insert_results = Some(queued);
    })?;
    h.start().await?;
    h.finish_as("fixture").await?;
    h.phase(Phase::Copied).await?;
    assert_eq!(h.snapshot().message, REMEDY);
    assert!(h.pasted.try_recv().is_err());
    h.start().await?;
    h.finish_as("pasted fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "pasted fixture");
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn saved_cleanup_opt_out_keeps_words_and_worker() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    h.start().await?;
    h.finish_as("Um, speak uh clearly.").await?;
    assert_eq!(receive(&h.pasted).await?, "speak clearly.");
    h.phase(Phase::Done).await?;
    let opted_out = Config {
        remove_fillers: false,
        ..Config::default()
    };
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    Config::save_changes(
        &[&Config::default()],
        &opted_out,
        &ValidConfig::assume(opted_out.clone()),
        &path,
    )?;
    h.runtime
        .configure(ValidConfig::assume(Config::read(&path)?));
    h.phase(Phase::Idle).await?;
    h.start().await?;
    h.finish_as("\nUm, speak uh literally.\n").await?;
    assert_eq!(receive(&h.pasted).await?, "Um, speak uh literally.");
    h.phase(Phase::Done).await?;
    assert!(
        h.loads.try_recv().is_err(),
        "Changing cleanup reloaded the model"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn dictation_inserts_once_and_request_preferences_keep_the_warm_worker() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    h.start().await?;
    h.finish_as("fixture one").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture one");
    h.phase(Phase::Done).await?;
    let config = Config {
        reduced_motion: true,
        preserve_clipboard: true,
        language: "auto".into(),
        ..Config::default()
    };
    h.runtime.configure(ValidConfig::assume(config));
    h.phase(Phase::Idle).await?;
    h.start().await?;
    h.finish_as("fixture two").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture two");
    h.phase(Phase::Done).await?;
    assert!(
        h.loads.try_recv().is_err(),
        "Request preferences reloaded the model"
    );
    assert!(h.pasted.try_recv().is_err(), "A result was submitted twice");
    let mut config = h.runtime.configuration.borrow().clone();
    config.engine = Engine::Parakeet;
    h.runtime.configure(ValidConfig::assume(config));
    receive(&h.loads).await?;
    h.start().await?;
    h.finish_as("new engine").await?;
    assert_eq!(receive(&h.pasted).await?, "new engine");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancelled_inference_and_late_audio_cannot_affect_the_next_session() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    let (old_id, old_events) = h.start().await?;
    let mut old_reply = h.finish().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    timeout(PATIENCE, old_reply.closed()).await?;
    assert!(
        answer(old_reply, "discard this").is_err(),
        "Cancelled inference stayed alive"
    );
    let (new_id, _) = h.start().await?;
    assert_ne!(new_id, old_id);
    let late_failure = CaptureEvent::Finished(old_id, Err(anyhow!("old device failed")));
    old_events.send(late_failure).await?;
    h.finish_as("new session").await?;
    assert_eq!(receive(&h.pasted).await?, "new session");
    h.phase(Phase::Done).await?;
    assert!(
        h.loads.try_recv().is_err(),
        "Cancelled GPU inference reloaded a healthy model"
    );
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn levels_after_a_capture_ends_never_reach_the_meter() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (old_id, old_events) = h.start().await?;
    old_events.send(CaptureEvent::Level(old_id, 0.25)).await?;
    h.observe_level(0.25).await?;
    old_events
        .send(CaptureEvent::Finished(old_id, Ok(None)))
        .await?;
    h.phase(Phase::Empty).await?;
    // The capture lane is ordered, so each phase below is published after the level before it.
    old_events.send(CaptureEvent::Level(old_id, 0.5)).await?;
    let (new_id, events) = h.start().await?;
    events.send(CaptureEvent::Level(old_id, 0.75)).await?;
    events
        .send(CaptureEvent::Finished(new_id, Ok(None)))
        .await?;
    h.phase(Phase::Empty).await?;
    assert!(
        !h.levels.has_changed()?,
        "A level from an ended capture reached the meter"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn failures_recover_the_worker_and_hook_loss_stops_the_owner() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    let (id, events) = h.start().await?;
    let disconnected = CaptureEvent::Finished(id, Err(anyhow!("Microphone disconnected")));
    events.send(disconnected).await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Microphone disconnected"));
    h.start().await?;
    let reply = h.finish().await?;
    assert!(reply.send(Err(anyhow!("Fixture request failed"))).is_ok());
    h.phase(Phase::Error).await?;
    receive(&h.loads).await?;
    assert!(h.snapshot().message.contains("Fixture request failed"));
    h.start().await?;
    h.runtime.input.close();
    h.phase(Phase::Error).await?;
    timeout(PATIENCE, h.exited()).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn monitor_failure_behind_a_queued_press_still_reports_its_remedy() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.runtime.input.fail("X".into());
    h.observe(|snapshot| snapshot.phase == Phase::Error && snapshot.message == "X")
        .await?;
    assert!(h.captures.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_lane_closed_behind_a_queued_press_still_reports_monitoring_stopped() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.runtime.input.close();
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Shortcut monitoring stopped"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn failed_recovery_stops_worker_before_loading_replacement() -> anyhow::Result<()> {
    let (stop, stopping) = gate(2);
    let (recover, recovery) = async_channel::bounded(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.stop = Some(stop);
        ports.recovery = Some(recovery);
    })?;
    receive(&h.loads).await?;
    h.start().await?;
    let mut old_reply = h.finish().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    timeout(PATIENCE, old_reply.closed()).await?;
    assert!(answer(old_reply, "discard this").is_err());
    recover.send(Err(anyhow!("Recovery failed"))).await?;
    stopping.reached().await?;
    assert!(
        still_pending(h.loads.recv()).await,
        "Replacement overlapped worker cleanup"
    );
    h.start().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Processing).await?;
    assert!(
        still_pending(h.jobs.recv()).await,
        "Inference began before recovery finished"
    );
    stopping.release().await?;
    receive(&h.loads).await?;
    h.transcribe_next_as("after recovery").await?;
    assert_eq!(receive(&h.pasted).await?, "after recovery");
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    h.runtime.request_stop();
    stopping.reached().await?;
    stopping.release().await?;
    h.close().await
}

#[tokio::test]
async fn stop_cancels_input_without_waiting_for_native_cleanup() -> anyhow::Result<()> {
    let (stop, stopping) = gate(1);
    let mut h = Harness::on_own_thread(Config::default(), |ports| ports.stop = Some(stop))?;
    h.start().await?;
    h.finish_as("fixture").await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;

    h.start().await?;
    h.runtime.request_stop();
    stopping.reached().await?;
    assert!(h.runtime.input.is_closed());
    assert!(!h.runtime.input.active());
    assert!(
        still_pending(h.runtime.stopped()).await,
        "Completion was reported before native cleanup finished"
    );
    stopping.release().await?;
    timeout(PATIENCE, h.runtime.stopped()).await?;
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn warmup_failure_while_stopping_retains_capture_and_retries_on_demand() -> anyhow::Result<()>
{
    let (results, loading) = async_channel::bounded(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.load_results = Some(loading);
        ports.delayed_finish = true;
    })?;
    receive(&h.loads).await?;
    let (id, events) = h.start().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Stopping).await?;
    results.send(Err(anyhow!("Model warmup failed"))).await?;
    h.observe(|snapshot| snapshot.message == "Model warmup failed")
        .await?;
    assert_eq!(h.snapshot().phase, Phase::Stopping);
    h.input(Input::Press);
    events.send(CaptureEvent::Level(id, 0.4)).await?;
    h.observe_level(0.4).await?;
    assert!(h.captures.try_recv().is_err());
    assert!(h.loads.try_recv().is_err(), "Failed warmup must not loop");
    events
        .send(CaptureEvent::Finished(id, Ok(Some(sealed()))))
        .await?;
    receive(&h.loads).await?;
    results.send(Ok(())).await?;
    h.transcribe_next_as("fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture");
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn loading_notice_clears_once_ready_and_applied_settings_are_confirmed() -> anyhow::Result<()>
{
    let (results, loading) = async_channel::bounded(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.load_results = Some(loading);
    })?;
    receive(&h.loads).await?;
    h.observe(|snapshot| snapshot.message == LOADING).await?;
    results.send(Ok(())).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    assert_eq!(h.snapshot().message, "");
    h.runtime.configure(ValidConfig::assume(Config {
        remove_fillers: false,
        ..Config::default()
    }));
    h.observe(|snapshot| snapshot.message == "Settings applied")
        .await?;
    assert_eq!(h.snapshot().phase, Phase::Idle);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn audio_awaiting_an_unavailable_worker_shows_loading_until_it_is_ready() -> anyhow::Result<()>
{
    let (results, loading) = async_channel::bounded(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.load_results = Some(loading);
    })?;
    receive(&h.loads).await?;
    results.send(Err(anyhow!("Initial load failed"))).await?;
    h.phase(Phase::Error).await?;
    assert_eq!(h.snapshot().message, "Initial load failed");
    h.start().await?;
    assert_eq!(h.snapshot().message, "");
    h.input(Input::Toggle);
    receive(&h.loads).await?;
    h.observe(|snapshot| snapshot.message == LOADING).await?;
    assert_eq!(h.snapshot().phase, Phase::Processing);
    results.send(Ok(())).await?;
    h.transcribe_next_as("fixture").await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn configuration_revokes_insertion_authority_before_owner_observation() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    h.start().await?;
    assert!(h.runtime.input.active());
    h.runtime.configure(ValidConfig::assume(Config {
        remove_fillers: false,
        ..Config::default()
    }));
    assert!(!h.runtime.input.active());
    h.phase(Phase::Idle).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn presentation_settings_keep_the_active_session() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    h.runtime.configure(ValidConfig::assume(Config {
        theme: Theme::default().next(),
        reduced_motion: true,
        ..Config::default()
    }));
    assert!(h.runtime.input.active());
    h.finish_as("fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture");
    h.phase(Phase::Done).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn retired_publisher_cannot_overwrite_shell_lifecycle_notice() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    h.output.send_modify(|snapshot| {
        snapshot.epoch += 1;
        snapshot.phase = Phase::Idle;
        snapshot.message = PAUSING.into();
    });
    h.input(Input::Toggle);
    h.transcribe_next_as("fixture").await?;
    receive(&h.pasted).await?;
    let updates = h.updates.clone();
    h.close().await?;
    assert_eq!(updates.borrow().message, PAUSING);
    assert_eq!(updates.borrow().phase, Phase::Idle);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn delayed_owner_start_cannot_claim_a_newer_shell_publisher() -> anyhow::Result<()> {
    let h = Harness::new()?;
    h.output.send_modify(|snapshot| {
        snapshot.epoch += 1;
        snapshot.message = PAUSED.into();
    });
    let updates = h.updates.clone();
    h.close().await?;
    assert_eq!(updates.borrow().message, PAUSED);
    Ok(())
}

const REPORT: &str = "Could not delete every expired Show";

#[tokio::test(start_paused = true)]
async fn an_idle_report_survives_model_load_and_desktop_readiness() -> anyhow::Result<()> {
    let (results, loading) = async_channel::bounded(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.load_results = Some(loading);
        ports.desktop_pending = true;
    })?;
    receive(&h.loads).await?;
    h.runtime
        .report(REPORT.into())
        .map_err(anyhow::Error::msg)?;
    h.phase(Phase::Error).await?;
    results.send(Ok(())).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    h.input(Input::DesktopReady {
        shortcut: "fixture shortcut".into(),
        cancel: "fixture cancel".into(),
    });
    h.observe(|snapshot| snapshot.desktop_ready).await?;
    assert_eq!(h.snapshot().phase, Phase::Error);
    assert_eq!(h.snapshot().message, REPORT);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_report_during_a_held_recording_is_dropped_and_the_text_still_pastes()
-> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.runtime
        .report(REPORT.into())
        .map_err(anyhow::Error::msg)?;
    // The report lane outranks capture, so the level arrives only after the report is handled.
    events.send(CaptureEvent::Level(id, 0.25)).await?;
    h.observe_level(0.25).await?;
    assert_eq!(h.snapshot().message, "", "The recording showed the report");
    tokio::time::advance(Duration::from_millis(250)).await;
    h.input(Input::Release);
    h.transcribe_next_as("fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture");
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_model_that_recovers_from_a_failed_warmup_clears_the_failure() -> anyhow::Result<()> {
    let (results, loading) = async_channel::bounded(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.load_results = Some(loading);
    })?;
    receive(&h.loads).await?;
    h.start().await?;
    results.send(Err(anyhow!("Initial warmup failed"))).await?;
    h.observe(|snapshot| snapshot.message == "Initial warmup failed")
        .await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    receive(&h.loads).await?;
    results.send(Ok(())).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    assert_eq!(
        h.snapshot().message,
        "",
        "A ready model still reports its failed warmup"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn recovery_failure_keeps_a_queued_dictation_until_microphone_retirement()
-> anyhow::Result<()> {
    let (results, loading) = async_channel::bounded(4);
    let (retirement, retiring) = gate(4);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.load_results = Some(loading);
        ports.retirement = Some(retirement);
    })?;
    receive(&h.loads).await?;
    h.start().await?;
    results.send(Err(anyhow!("Initial warmup failed"))).await?;
    h.observe(|snapshot| snapshot.message == "Initial warmup failed")
        .await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    retiring.reached().await?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.phase(Phase::Starting).await?;
    results.send(Err(anyhow!("Recovery failed"))).await?;
    h.observe(|snapshot| snapshot.message == "Recovery failed")
        .await?;
    assert_eq!(h.snapshot().phase, Phase::Starting);
    assert!(h.captures.try_recv().is_err());
    retiring.release().await?;
    receive(&h.captures).await?;
    h.phase(Phase::Recording).await?;
    h.input(Input::Toggle);
    receive(&h.loads).await?;
    results.send(Ok(())).await?;
    h.transcribe_next_as("fixture").await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().message, "");
    retiring.reached().await?;
    retiring.release().await?;
    h.close().await
}

#[tokio::test]
#[ignore = "Reports owner reply-to-insertion latency with fake devices; scheduler noise must not gate it"]
async fn profile_ready_text_latency() -> anyhow::Result<()> {
    let mut h = Harness::on_own_thread(Config::default(), |_| {})?;
    receive(&h.loads).await?;
    let mut samples = Vec::with_capacity(100);
    for _ in 0..100 {
        h.start().await?;
        let reply = h.finish().await?;
        let started = Instant::now();
        answer(reply, "fixture")?;
        receive(&h.pasted).await?;
        samples.push(started.elapsed().as_micros());
        h.phase(Phase::Done).await?;
    }
    samples.sort_unstable();
    eprintln!(
        "Ready text -> fake insertion: median={}us p95={}us n={}",
        samples[50],
        samples[95],
        samples.len()
    );
    Ok(())
}
