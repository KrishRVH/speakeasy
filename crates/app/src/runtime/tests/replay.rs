use super::*;

#[tokio::test(start_paused = true)]
async fn unavailable_insertion_keeps_exact_text_for_a_fresh_delivery() -> anyhow::Result<()> {
    let (results, pending) = async_channel::bounded(1);
    results
        .send(Inserted::Unavailable("Fixture target unavailable."))
        .await?;
    let mut h = Harness::with_ports(english(), |ports| {
        ports.insert_results = Some(pending);
    })?;
    h.start().await?;
    h.finish_as("Um, retained fixture.").await?;
    h.phase(Phase::Error).await?;
    let first = h.snapshot();
    assert!(first.last_dictation);
    let remedy = if cfg!(target_os = "linux") {
        "Paste last dictation"
    } else {
        "press V to paste them"
    };
    assert!(first.message.contains(remedy), "{}", first.message);
    h.input(Input::PasteLast);
    assert_eq!(receive(&h.pasted).await?, "retained fixture.");
    h.phase(Phase::Done).await?;
    assert!(h.snapshot().session > first.session);
    assert!(h.captures.try_recv().is_err());
    assert!(h.jobs.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn paste_during_a_hold_discards_its_audio_before_replaying() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    h.finish_as("Kept fixture.").await?;
    assert_eq!(receive(&h.pasted).await?, "Kept fixture.");
    h.phase(Phase::Done).await?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::PasteLast);
    h.input(Input::Release);
    assert_eq!(receive(&h.pasted).await?, "Kept fixture.");
    h.phase(Phase::Done).await?;
    assert!(h.jobs.try_recv().is_err());
    assert!(h.captures.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn paste_on_the_press_that_finishes_hands_free_keeps_that_dictation() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    h.finish_as("Kept fixture.").await?;
    assert_eq!(receive(&h.pasted).await?, "Kept fixture.");
    h.phase(Phase::Done).await?;
    h.input(Input::Toggle);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Press);
    h.input(Input::PasteLast);
    h.input(Input::Release);
    h.transcribe_next_as("Hands-free fixture.").await?;
    assert_eq!(
        receive(&h.pasted).await?,
        "Hands-free fixture.",
        "Paste last discarded the hands-free dictation it was finishing"
    );
    h.phase(Phase::Done).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancelled_text_never_replaces_recall_and_settings_preserve_it() -> anyhow::Result<()> {
    let (insertion, preparing) = gate(4);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.insertion = Some(insertion);
    })?;
    h.start().await?;
    h.finish_as("Kept fixture.").await?;
    preparing.reached().await?;
    preparing.release().await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    h.start().await?;
    h.finish_as("Cancelled fixture.").await?;
    let cancelled = preparing.reached().await?;
    h.input(Input::Escape);
    h.phase(Phase::Cancelled).await?;
    assert!(!cancelled.active());
    preparing.release().await?;
    h.input(Input::PasteLast);
    preparing.reached().await?;
    preparing.release().await?;
    assert_eq!(receive(&h.pasted).await?, "Kept fixture.");
    h.phase(Phase::Done).await?;
    change_settings(&mut h);
    h.phase(Phase::Idle).await?;
    assert!(h.snapshot().last_dictation);
    h.input(Input::PasteLast);
    preparing.reached().await?;
    preparing.release().await?;
    assert_eq!(receive(&h.pasted).await?, "Kept fixture.");
    h.phase(Phase::Done).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn empty_replay_during_a_hold_preserves_the_recording() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    tokio::time::advance(Duration::from_millis(400)).await;
    h.input(Input::PasteLast);
    h.input(Input::Release);
    h.transcribe_next_as("Current fixture.").await?;
    assert_eq!(receive(&h.pasted).await?, "Current fixture.");
    h.phase(Phase::Done).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_dictation_that_loses_its_microphone_waits_for_paste_last() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, events) = h.start().await?;
    events.send(disconnected(id)).await?;
    h.transcribe_next_as("Words before the cable came out.")
        .await?;
    h.phase(Phase::Notice).await?;
    let notice = h.snapshot();
    assert!(notice.last_dictation);
    let route = if cfg!(target_os = "linux") {
        "or run speakeasy paste-last."
    } else {
        "press V to paste your words."
    };
    assert!(
        notice
            .message
            .starts_with("The microphone was disconnected.")
            && notice.message.ends_with(route),
        "{}",
        notice.message
    );
    assert!(
        h.take_sounds().contains(&Sound::Played(Cue::Stop)),
        "The recording ended where its microphone stopped"
    );
    assert!(
        h.pasted.try_recv().is_err(),
        "Words from an interrupted dictation reached the focused app"
    );
    h.input(Input::PasteLast);
    assert_eq!(
        receive(&h.pasted).await?,
        "Words before the cable came out."
    );
    h.phase(Phase::Done).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_lost_microphone_with_no_words_fails_with_its_remedy() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, events) = h.start().await?;
    events.send(disconnected(id)).await?;
    h.transcribe_next_as("").await?;
    h.phase(Phase::Error).await?;
    let failed = h.snapshot();
    assert_eq!(
        failed.message,
        "The microphone was disconnected. Reconnect it."
    );
    assert!(!failed.last_dictation);
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}
