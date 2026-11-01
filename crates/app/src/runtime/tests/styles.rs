use super::*;
use crate::transcript::style::Case;

#[tokio::test(start_paused = true)]
async fn style_is_fixed_when_recording_starts_and_replay_never_reformats() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.styles.send_replace(Style::Identifier(Case::Snake));
    h.start().await?;
    h.styles.send_replace(Style::Identifier(Case::Camel));
    h.finish_as("Um, HTTP server 2.").await?;
    assert_eq!(receive(&h.pasted).await?, "http_server_2");
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().style, Style::Identifier(Case::Snake));
    h.input(Input::PasteLast);
    assert_eq!(receive(&h.pasted).await?, "http_server_2");
    h.phase(Phase::Done).await?;
    h.start().await?;
    h.finish_as("HTTP server 2.").await?;
    assert_eq!(receive(&h.pasted).await?, "httpServer2");
    h.phase(Phase::Done).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn verbatim_preserves_fillers_and_practice_uses_the_selected_style() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.styles.send_replace(Style::Verbatim);
    let mut practice = arm_practice(&h).await?;
    h.start().await?;
    h.finish_as("Um, keep uh these.").await?;
    let text = practice_text(&mut practice).await?;
    assert_eq!(&*text, "Um, keep uh these.");
    h.phase(Phase::Done).await?;
    assert!(!h.snapshot().last_dictation);
    h.close().await
}
