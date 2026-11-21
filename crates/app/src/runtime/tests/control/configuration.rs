use super::*;

#[tokio::test(start_paused = true)]
async fn a_rollback_after_a_show_keeps_the_failure_that_cost_its_narration() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.ready().await?;
    let saved = h.runtime.baseline.clone();
    let french = ValidConfig::assume(Config {
        language: "fr".into(),
        ..saved.clone()
    });
    assert_eq!(receive(&h.runtime.adopt(french.clone())).await?, Ok(()));
    h.runtime.settle(french);
    start_show(&mut h).await?;
    assert!(h.runtime.revert(saved));
    finish_show(&h);
    let (_, reply) = receive(&h.jobs).await?;
    assert!(reply.send(Err(anyhow!("Engine exited"))).is_ok());
    receive(&h.loads).await?;
    let (_, retry) = receive(&h.jobs).await?;
    assert!(retry.send(Err(anyhow!("Engine exited again"))).is_ok());
    receive(&h.published).await?;
    h.phase(Phase::Copied).await?;
    control(&h, Command::Status).await?;
    assert_eq!(
        h.snapshot().message,
        "Show copied without narration. Engine exited again",
        "The rollback that waited out a Show dropped why it lost its narration"
    );
    h.close().await
}
