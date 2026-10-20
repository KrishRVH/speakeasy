//! Show scenarios: S during a hold, screen lanes, footage, and publication, against a fake
//! screen and gated publication.

use super::*;

/// Holds the shortcut and presses S, returning the screen recorder's lane.
async fn start_show(h: &mut Harness) -> anyhow::Result<OpenedShow> {
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show);
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    receive(&h.shows).await
}

#[tokio::test(start_paused = true)]
async fn s_during_a_hold_turns_the_recording_into_a_show_and_publishes_it() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    start_show(&mut h).await?;
    // The Show is locked hands-free: releasing the shortcut keeps recording.
    h.input(Input::Release);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(h.jobs.try_recv().is_err());
    h.input(Input::Press);
    let (_, reply) = receive(&h.jobs).await?;
    answer(reply, "look here")?;
    let (footage, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("look here".into()));
    assert_eq!(footage.journal.samples().len(), 1);
    h.phase(Phase::Copied).await?;
    assert!(h.snapshot().message.contains("Show copied"));
    assert!(h.pasted.try_recv().is_err(), "A Show never pastes");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_silent_show_still_publishes_its_screen() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| ports.delayed_finish = true)?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Show);
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
async fn cancelling_a_show_publishes_nothing_and_frees_the_screen() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    start_show(&mut h).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(h.published.try_recv().is_err());
    // The next Show opens a new screen recorder.
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
    publishing.release().await?;
    assert!(still_pending(h.published.recv()).await);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_that_cannot_be_published_explains_why() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publish_error = Some("disk full");
    })?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("look").await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("disk full"));
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
        ports.show_error = Some("screen recording denied");
    })?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    h.input(Input::Show);
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("screen recording denied"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn s_outside_a_hold_records_no_screen() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Show);
    h.start().await?;
    h.input(Input::Show);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(h.shows.try_recv().is_err());
    assert!(!h.snapshot().show);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_microphone_failure_ends_the_show_and_frees_the_screen() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Show);
    receive(&h.shows).await?;
    events
        .send(CaptureEvent::Finished(
            id,
            Err(anyhow!("Microphone disconnected")),
        ))
        .await?;
    h.phase(Phase::Error).await?;
    // The screen recorder retired with the session, so the next Show records again.
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_stops_recording_explains_why_and_frees_the_microphone() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    let (id, events) = start_show(&mut h).await?;
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

#[tokio::test(start_paused = true)]
async fn footage_from_an_earlier_show_never_reaches_a_later_one() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (earlier, stale) = start_show(&mut h).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    let mut late = footage();
    late.ended = Moment::from_millis(9_999);
    stale.send(ShowEvent::Finished(earlier, Ok(late))).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("this one").await?;
    let (published, _) = receive(&h.published).await?;
    assert_eq!(published.ended, footage().ended);
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
    // The second Show's narration ends before the first screen recorder lets go.
    h.input(Input::Release);
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show);
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("second").await?;
    assert!(still_pending(h.shows.recv()).await);
    retiring.release().await?;
    receive(&h.shows).await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("second".into()));
    retiring.reached().await?;
    retiring.release().await?;
    h.close().await
}
