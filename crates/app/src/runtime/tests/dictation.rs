use super::*;
use crate::{
    config::{Engine, TextDelivery},
    theme::Theme,
    transcript::vocabulary::Correction,
};

#[tokio::test(start_paused = true)]
async fn desktop_readiness_blocks_capture_until_native_startup_completes() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| ports.desktop_pending = true)?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.input(Input::Toggle);
    h.input(Input::StartShow(Scope::Screen));
    h.input(Input::DesktopReady {
        shortcut: "fixture shortcut".into(),
        cancel: "fixture cancel".into(),
        globe_shortcut: false,
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
    tokio::time::advance(Duration::from_millis(400)).await;
    h.input(Input::Release);
    h.transcribe_next_as("").await?;
    h.phase(Phase::Empty).await?;
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn return_during_a_hold_sends_only_that_dictation() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    h.input(Input::Submit);
    h.transcribe_next_as("Ship it.").await?;
    assert_eq!(receive(&h.entered).await?, "Ship it.");
    h.phase(Phase::Done).await?;
    h.start().await?;
    h.input(Input::Submit);
    h.finish_as("Hands free.").await?;
    assert_eq!(receive(&h.pasted).await?, "Hands free.");
    assert!(h.entered.try_recv().is_err());
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
    tokio::time::advance(limit.checked_sub(Duration::from_millis(1)).unwrap()).await;
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
        ports.microphone_error = Some("Thread resources exhausted");
    })?;
    h.input(Input::Press);
    h.phase(Phase::Error).await?;
    let snapshot = h.snapshot();
    assert!(snapshot.message.contains("Thread resources exhausted"));
    assert_eq!(snapshot.trouble, Some("Microphone unavailable"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancelled_capture_retires_before_retry_and_shutdown_waits_for_it() -> anyhow::Result<()> {
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
    h.input(Input::Escape);
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
async fn silence_and_empty_recognition_never_report_submission() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(english(), |_| {})?;
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
        text_delivery: TextDelivery::Type,
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
    let mut config = h.runtime.baseline.clone();
    config.engine = Engine::Whisper;
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
    h.input(Input::Escape);
    h.phase(Phase::Cancelled).await?;
    timeout(PATIENCE, old_reply.closed()).await?;
    assert!(
        answer(old_reply, "discard this").is_err(),
        "Cancelled inference stayed alive"
    );
    let (new_id, _) = h.start().await?;
    assert_ne!(new_id, old_id);
    let late_failure = CaptureEvent::Finished(old_id, Err(anyhow!("Old device failed")));
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
async fn failed_transcription_retries_its_speech_on_a_fresh_worker() -> anyhow::Result<()> {
    let (results, loading) = async_channel::bounded(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.load_results = Some(loading);
    })?;
    receive(&h.loads).await?;
    results.send(Ok(())).await?;
    h.start().await?;
    let reply = h.finish().await?;
    assert!(reply.send(Err(anyhow!("Engine exited"))).is_ok());
    receive(&h.loads).await?;
    h.observe(|snapshot| snapshot.message == LOADING).await?;
    results.send(Ok(())).await?;
    h.transcribe_next_as("kept").await?;
    assert_eq!(receive(&h.pasted).await?, "kept");
    h.phase(Phase::Done).await?;
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
async fn presentation_settings_keep_the_active_session() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    h.runtime.configure(ValidConfig::assume(Config {
        theme: Theme::Emerald,
        reduced_motion: true,
        ..Config::default()
    }));
    assert!(h.runtime.input.active());
    h.finish_as("fixture").await?;
    assert_eq!(receive(&h.pasted).await?, "fixture");
    h.phase(Phase::Done).await?;
    h.close().await
}

const REPORT: &str = "Can't delete every expired Show";

#[tokio::test(start_paused = true)]
async fn only_a_recurring_failure_that_repeats_is_not_new_feedback() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.report(Report::Recurring(REPORT.into()))?;
    h.phase(Phase::Error).await?;
    h.report(Report::Recurring(REPORT.into()))?;
    assert!(
        still_pending(h.observe(|snapshot| snapshot.reported != 0)).await,
        "A sweep's unchanged failure popped up again"
    );
    h.report(Report::Failed(REPORT.into()))?;
    h.observe(|snapshot| snapshot.reported == 1).await?;
    h.close().await
}

#[tokio::test]
#[ignore = "Reports owner reply-to-insertion latency with fake devices; scheduler noise must not gate it"]
async fn profile_ready_text_latency() -> anyhow::Result<()> {
    const RUNS: usize = 100;
    let mut h = Harness::on_own_thread(Config::default(), |_| {})?;
    receive(&h.loads).await?;
    let mut samples = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
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
        "Ready text -> fake insertion: median={}us p95={}us n={RUNS}",
        samples[RUNS / 2],
        samples[RUNS * 95 / 100],
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn phrase_corrections_follow_filler_cleanup_for_dictation_and_shows_without_reloading()
-> anyhow::Result<()> {
    let mut h = Harness::with_ports(english(), |_| {})?;
    receive(&h.loads).await?;
    h.start().await?;
    h.finish_as("Ready.").await?;
    assert_eq!(receive(&h.pasted).await?, "Ready.");
    h.phase(Phase::Done).await?;
    let config = Config {
        vocabulary: vec![Correction {
            heard: "cube control".into(),
            written: "kubectl".into(),
        }],
        ..english()
    };
    h.runtime.configure(ValidConfig::assume(config));
    h.phase(Phase::Idle).await?;
    h.start().await?;
    h.finish_as("Use cube um control.").await?;
    assert_eq!(receive(&h.pasted).await?, "Use kubectl.");
    h.phase(Phase::Done).await?;
    start_show(&mut h).await?;
    finish_show(&h);
    h.transcribe_next_as("Cube uh control!").await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("kubectl!".into()));
    assert!(
        h.loads.try_recv().is_err(),
        "Phrase corrections reloaded the speech model"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn the_default_microphone_is_named_only_for_the_session_that_fell_back() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    let (id, events) = h.start().await?;
    assert!(!h.snapshot().default_microphone);
    events
        .send(CaptureEvent::Ready(id, MicrophoneSource::Default))
        .await?;
    h.observe(|snapshot| snapshot.default_microphone).await?;
    h.finish_as("Fallback words.").await?;
    assert_eq!(receive(&h.pasted).await?, "Fallback words.");
    h.phase(Phase::Done).await?;
    h.start().await?;
    assert!(!h.snapshot().default_microphone);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn the_computer_stays_awake_until_the_words_are_delivered() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    assert_eq!(receive(&h.awake).await?, Some(Keep::System));
    let reply = h.finish().await?;
    h.phase(Phase::Processing).await?;
    assert!(
        h.awake.try_recv().is_err(),
        "Recognition ran without the guard"
    );
    answer(reply, "Awake words.")?;
    assert_eq!(receive(&h.pasted).await?, "Awake words.");
    h.phase(Phase::Done).await?;
    assert_eq!(receive(&h.awake).await?, None);
    h.close().await
}
