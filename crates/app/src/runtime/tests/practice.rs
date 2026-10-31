//! Practice uses the real owner and fake devices, defending its private destination and lifetime.

use super::*;

async fn arm<Host: Sync>(
    h: &Harness<Host>,
) -> anyhow::Result<tokio::sync::mpsc::Receiver<PracticeEvent>> {
    let mut receipt = h.runtime.practice().map_err(anyhow::Error::msg)?;
    assert!(matches!(
        timeout(PATIENCE, receipt.recv()).await?,
        Some(PracticeEvent::Ready)
    ));
    Ok(receipt)
}

#[tokio::test(start_paused = true)]
async fn practice_keeps_text_private_and_cannot_promote_to_a_show() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let mut receipt = arm(&h).await?;
    let (id, events) = h.start().await?;
    h.input(Input::Show(Scope::Screen));
    events.send(CaptureEvent::Level(id, 0.25)).await?;
    h.observe_level(0.25).await?;
    assert!(h.shows.try_recv().is_err());
    h.finish_as("Private practice words").await?;
    let Some(PracticeEvent::Text(text)) = timeout(PATIENCE, receipt.recv()).await? else {
        panic!("Practice did not receive its transcript");
    };
    assert_eq!(&*text, "Private practice words");
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    assert!(!format!("{:?}", h.snapshot()).contains("Private practice words"));
    assert!(h.snapshot().message.contains("Practice complete"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn closing_practice_revokes_processing_and_the_next_session_is_external() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    let receipt = arm(&h).await?;
    h.start().await?;
    let late = h.finish().await?;
    drop(receipt);
    h.phase(Phase::Cancelled).await?;
    drop(late);
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    assert!(h.pasted.try_recv().is_err());
    h.start().await?;
    h.finish_as("Next session").await?;
    assert_eq!(receive(&h.pasted).await?, "Next session");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancellation_wins_over_a_ready_practice_transcript() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let mut receipt = arm(&h).await?;
    h.start().await?;
    let reply = h.finish().await?;
    answer(reply, "Must not arrive")?;
    // Native cancellation revokes authority before its event reaches the owner.
    h.runtime.input.cancel();
    h.phase(Phase::Cancelled).await?;
    assert!(receipt.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn practice_cannot_take_over_an_existing_recording() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    let mut receipt = h.runtime.practice().map_err(anyhow::Error::msg)?;
    assert!(matches!(
        timeout(PATIENCE, receipt.recv()).await?,
        Some(PracticeEvent::Unavailable(_))
    ));
    h.finish_as("Original destination").await?;
    assert_eq!(receive(&h.pasted).await?, "Original destination");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn reconfiguration_closes_practice_and_cancels_its_capture() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let mut receipt = arm(&h).await?;
    h.start().await?;
    change_settings(&h);
    assert!(timeout(PATIENCE, receipt.recv()).await?.is_none());
    h.phase(Phase::Cancelled).await?;
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn an_explicit_show_disarms_idle_practice() -> anyhow::Result<()> {
    let h = Harness::new()?;
    let mut receipt = arm(&h).await?;
    h.input(Input::StartShow(Scope::Screen));
    assert!(timeout(PATIENCE, receipt.recv()).await?.is_none());
    receive(&h.shows).await?;
    h.input(Input::Press);
    h.input(Input::Escape);
    h.close().await
}
