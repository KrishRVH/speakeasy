//! Show scenarios: S, W, or A during a hold, screen lanes, footage, and publication, against a fake
//! screen and gated publication.

use speakeasy_show::Ending;

use super::{ports::PUBLISH_STOPPED, *};

/// Holds the shortcut and presses S, returning the screen recorder's lane.
async fn start_show(h: &mut Harness) -> anyhow::Result<OpenedShow> {
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    receive(&h.shows).await
}

#[tokio::test(start_paused = true)]
async fn s_during_a_hold_turns_the_recording_into_a_show_and_publishes_it() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(
        h.jobs.try_recv().is_err(),
        "Releasing the shortcut ended a hands-free Show"
    );
    h.input(Input::Press);
    h.transcribe_next_as("look here").await?;
    let (footage, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("look here".into()));
    assert_eq!(footage.journal.samples().len(), 1);
    h.phase(Phase::Copied).await?;
    let snapshot = h.snapshot();
    assert!(snapshot.message.contains("Show copied"));
    assert_eq!(snapshot.ending, Ending::Finished);
    assert!(h.pasted.try_recv().is_err(), "A Show never pastes");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn w_and_a_record_the_window_or_area_they_name() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    for scope in [Scope::Window, Scope::Area] {
        h.input(Input::Press);
        h.phase(Phase::Recording).await?;
        receive(&h.captures).await?;
        h.input(Input::Show(scope));
        let (_, recorded, _) = receive(&h.shows).await?;
        assert_eq!(recorded, scope);
        h.input(Input::Cancel);
        h.phase(Phase::Cancelled).await?;
        h.input(Input::Release);
    }
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_pin_during_a_show_reaches_its_screen_recorder() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, ..) = start_show(&mut h).await?;
    let taps = double_tap();
    h.input(Input::Pin(taps));
    assert_eq!(receive(&h.pins).await?, (id, taps));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_silent_show_still_publishes_its_screen() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| ports.delayed_finish = true)?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    events.send(CaptureEvent::Finished(id, Ok(None))).await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::default());
    assert!(h.jobs.try_recv().is_err(), "Silence is not transcribed");
    h.phase(Phase::Copied).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_show_frees_the_screen_for_the_next() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    start_show(&mut h).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancelling_while_publishing_leaves_no_trace() -> anyhow::Result<()> {
    let (publication, publishing) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publication = Some(publication);
    })?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("this broke").await?;
    publishing.reached().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.runtime.request_stop();
    assert!(
        still_pending(h.exited()).await,
        "Shutdown left the cancelled publication running"
    );
    publishing.release().await?;
    timeout(PATIENCE, h.exited()).await?;
    assert!(
        h.published.try_recv().is_err(),
        "A cancelled Show was published"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_that_cannot_be_published_explains_why() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publish_error = Some("Disk full");
    })?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("look").await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Disk full"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_publication_that_panics_reports_the_fixed_show_failure() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| ports.publish_panics = true)?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("look").await?;
    h.phase(Phase::Error).await?;
    assert_eq!(h.snapshot().message, PUBLISH_STOPPED);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn shutdown_awaits_a_retiring_screen() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.screen_retirement = Some(retirement);
    })?;
    start_show(&mut h).await?;
    h.runtime.request_stop();
    retiring.reached().await?;
    assert!(still_pending(h.exited()).await);
    retiring.release().await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn shutdown_revokes_and_owns_pending_publication_cleanup() -> anyhow::Result<()> {
    let (publication, publishing) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publication = Some(publication);
    })?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("fixture").await?;
    publishing.reached().await?;
    h.runtime.request_stop();
    assert!(still_pending(h.exited()).await);
    publishing.release().await?;
    timeout(PATIENCE, h.exited()).await?;
    assert!(
        h.published.try_recv().is_err(),
        "A revoked Show was published"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_cannot_be_recorded_explains_why() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.screen_error = Some("Screen recording denied");
    })?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    h.input(Input::Show(Scope::Screen));
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Screen recording denied"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn s_with_no_recording_records_no_screen_then_or_later() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Show(Scope::Screen));
    h.start().await?;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(h.shows.try_recv().is_err());
    assert!(!h.snapshot().show);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn s_after_space_locked_the_hold_still_starts_a_show() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Lock);
    h.observe(|snapshot| snapshot.hands_free).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show).await?;
    receive(&h.shows).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("locked").await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("locked".into()));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn s_while_the_last_microphone_retires_still_starts_a_show() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(2);
    let (screen_retirement, screen_retiring) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
        ports.screen_retirement = Some(screen_retirement);
    })?;
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    retiring.reached().await?;
    h.input(Input::Press);
    h.phase(Phase::Starting).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    receive(&h.shows).await?;
    assert!(
        still_pending(screen_retiring.reached()).await,
        "The screen stopped before the Show's narration began"
    );
    retiring.release().await?;
    receive(&h.captures).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("queued").await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("queued".into()));
    for gate in [&retiring, &screen_retiring] {
        gate.reached().await?;
        gate.release().await?;
    }
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_microphone_failure_ends_the_show_and_frees_the_screen() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    events
        .send(CaptureEvent::Finished(
            id,
            Err(anyhow!("Microphone disconnected")),
        ))
        .await?;
    h.phase(Phase::Error).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_stops_recording_explains_why_and_frees_the_microphone() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    let (id, _, events) = start_show(&mut h).await?;
    events
        .send(ShowEvent::Finished(
            id,
            Err(anyhow!("Screen recording is off")),
        ))
        .await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Screen recording is off"));
    h.input(Input::Release);
    h.input(Input::Press);
    receive(&h.captures).await?;
    h.close().await
}

/// Footage whose screen stopped recording before the user finished the Show.
fn stopped_footage() -> Footage {
    let mut footage = footage();
    footage.recording.ending = Ending::ScreenStopped;
    footage
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_stops_partway_finishes_the_show_and_says_so() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, _, events) = start_show(&mut h).await?;
    events
        .send(ShowEvent::Finished(id, Ok(stopped_footage())))
        .await?;
    h.transcribe_next_as("before it stopped").await?;
    let (published, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("before it stopped".into()));
    assert_eq!(published.recording, stopped_footage().recording);
    h.phase(Phase::Copied).await?;
    let snapshot = h.snapshot();
    assert_eq!(
        snapshot.ending,
        Ending::ScreenStopped,
        "The pill presents the notice"
    );
    assert_eq!(
        snapshot.message,
        "Show copied, but the screen stopped recording early."
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_show_whose_screen_stopped_publishes_nothing() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, _, events) = start_show(&mut h).await?;
    events
        .send(ShowEvent::Finished(id, Ok(stopped_footage())))
        .await?;
    let (_, mut reply) = receive(&h.jobs).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    timeout(PATIENCE, reply.closed()).await?;
    assert!(
        answer(reply, "too late").is_err(),
        "The cancelled Show's transcription stayed alive"
    );
    assert!(
        still_pending(h.published.recv()).await,
        "A cancelled Show was published"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn footage_from_an_earlier_show_never_reaches_a_later_one() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (earlier, _, stale) = start_show(&mut h).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    let mut late = footage();
    late.recording.duration = Moment::from_millis(9_999);
    stale.send(ShowEvent::Finished(earlier, Ok(late))).await?;
    assert!(
        still_pending(h.published.recv()).await,
        "A Show published while it was still recording"
    );
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("this one").await?;
    let (published, _) = receive(&h.published).await?;
    assert_eq!(published.recording, footage().recording);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_queued_behind_a_retiring_screen_still_records_it() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.screen_retirement = Some(retirement);
    })?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("first").await?;
    receive(&h.published).await?;
    retiring.reached().await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("second").await?;
    assert!(
        still_pending(h.shows.recv()).await,
        "The second Show's screen opened before the first let go"
    );
    retiring.release().await?;
    receive(&h.shows).await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("second".into()));
    retiring.reached().await?;
    retiring.release().await?;
    h.close().await
}
