use super::*;

#[tokio::test(start_paused = true)]
async fn unavailable_insertion_keeps_exact_text_for_a_fresh_delivery() -> anyhow::Result<()> {
    let (results, pending) = async_channel::bounded(1);
    results
        .send(Inserted::Unavailable("Fixture target unavailable."))
        .await?;
    let mut h = Harness::with_ports(
        Config {
            language: "en".into(),
            ..Config::default()
        },
        |ports| ports.insert_results = Some(pending),
    )?;
    h.start().await?;
    h.finish_as("Um, retained fixture.").await?;
    h.phase(Phase::Error).await?;
    let first = h.snapshot();
    assert!(first.last_dictation);
    assert!(first.message.contains("Paste last dictation"));
    assert!(!format!("{first:?}").contains("retained fixture"));
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
async fn replay_owns_delivery_until_completion_or_escape() -> anyhow::Result<()> {
    let (insertion, preparing) = gate(3);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.insertion = Some(insertion);
    })?;
    h.start().await?;
    h.finish_as("First fixture.").await?;
    let original = preparing.reached().await?;
    preparing.release().await?;
    assert_eq!(receive(&h.pasted).await?, "First fixture.");
    h.phase(Phase::Done).await?;
    h.input(Input::PasteLast);
    let replay = preparing.reached().await?;
    assert!(replay.active());
    assert!(!original.active());
    h.input(Input::Press);
    h.input(Input::PasteLast);
    h.input(Input::Release);
    h.input(Input::Escape);
    h.phase(Phase::Cancelled).await?;
    assert!(!replay.active());
    assert!(h.captures.try_recv().is_err());
    preparing.release().await?;
    h.input(Input::PasteLast);
    let next = preparing.reached().await?;
    assert!(next.active());
    assert!(!replay.commit());
    preparing.release().await?;
    assert_eq!(receive(&h.pasted).await?, "First fixture.");
    h.phase(Phase::Done).await?;
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
    let mut config = h.runtime.configuration.borrow().clone();
    config.language = "fr".into();
    h.runtime.configure(ValidConfig::assume(config));
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
async fn practice_cannot_populate_or_replay_recall() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let mut practice = arm_practice(&h).await?;
    h.input(Input::PasteLast);
    h.start().await?;
    h.finish_as("Private practice fixture.").await?;
    assert_eq!(
        &*practice_text(&mut practice).await?,
        "Private practice fixture."
    );
    h.phase(Phase::Done).await?;
    assert!(!h.snapshot().last_dictation);
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn empty_replay_during_a_hold_preserves_the_recording() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    tokio::time::advance(Duration::from_millis(300)).await;
    h.input(Input::PasteLast);
    h.input(Input::Release);
    let (_, reply) = receive(&h.jobs).await?;
    answer(reply, "Current fixture.")?;
    assert_eq!(receive(&h.pasted).await?, "Current fixture.");
    h.phase(Phase::Done).await?;
    h.close().await
}
