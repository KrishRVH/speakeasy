use speakeasy_core::machine::Refusal;

use super::*;
use crate::{runtime::owner::commands::refusal, theme::Theme};

async fn choose(
    h: &Harness,
    name: &str,
    config: &Config,
    baseline: &Config,
    session: u64,
) -> anyhow::Result<Reply> {
    let (call, receipt) = Call::new(Command::Preset { name: name.into() });
    h.runtime.preset(
        name.into(),
        ValidConfig::assume(config.clone()),
        baseline.clone(),
        session,
        call,
    );
    receive(&receipt).await
}

#[tokio::test(start_paused = true)]
async fn a_preset_applies_only_while_idle_and_reloads_only_for_engine_choices() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    h.ready().await?;
    let baseline = h.runtime.baseline.clone();
    let french = Config {
        language: "fr".into(),
        ..baseline.clone()
    };
    let adopted = choose(&h, "French", &french, &baseline, h.snapshot().session).await?;
    assert_eq!(adopted.result, Disposition::Ok);
    assert!(h.loads.try_recv().is_err());
    h.start().await?;
    let busy = choose(&h, "Busy", &baseline, &baseline, h.snapshot().session).await?;
    assert_eq!(busy.reason.as_deref(), Some(refusal(Refusal::PresetBusy)));
    finish_in(&mut h, "fr").await?;
    let faster = Config {
        threads: french.threads + 1,
        ..french
    };
    let adopted = choose(&h, "Faster", &faster, &baseline, h.snapshot().session).await?;
    assert_eq!(adopted.result, Disposition::Ok);
    h.ready().await?;
    assert!(h.loads.try_recv().is_err());
    assert_eq!(h.snapshot().preset.as_deref(), Some("Faster"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn resumed_preset_keeps_saved_settings_separate_from_effective_choices() -> anyhow::Result<()>
{
    let saved = Config::default();
    let effective = Config {
        language: "fr".into(),
        ..saved.clone()
    };
    let mut h = Harness::resuming(effective, saved.clone())?;
    h.ready().await?;
    h.start().await?;
    h.runtime.configure(ValidConfig::assume(saved.clone()));
    finish_in(&mut h, "fr").await?;
    let adopted = choose(&h, "Default", &saved, &saved, h.snapshot().session).await?;
    assert_eq!(adopted.result, Disposition::Ok);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn adopting_the_saved_settings_clears_the_preset_and_uses_the_saved_language()
-> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.ready().await?;
    let saved = h.runtime.baseline.clone();
    let french = Config {
        language: "fr".into(),
        ..saved.clone()
    };
    let adopted = choose(&h, "French", &french, &saved, h.snapshot().session).await?;
    assert_eq!(adopted.result, Disposition::Ok);
    h.observe(|snapshot| snapshot.preset.as_deref() == Some("French"))
        .await?;

    let saved = ValidConfig::assume(saved);
    assert_eq!(receive(&h.runtime.adopt(saved.clone())).await?, Ok(()));
    h.observe(|snapshot| snapshot.preset.is_none()).await?;
    h.runtime.settle(saved.clone());
    h.start().await?;
    finish_in(&mut h, &saved.language).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_session_started_after_adoption_survives_the_save_that_follows() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.ready().await?;
    let french = ValidConfig::assume(Config {
        language: "fr".into(),
        ..h.runtime.baseline.clone()
    });
    assert_eq!(receive(&h.runtime.adopt(french.clone())).await?, Ok(()));
    h.start().await?;
    h.runtime.settle(french.clone());
    h.runtime.configure(french);
    finish_in(&mut h, "fr").await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_rollback_waits_out_the_show_that_a_theme_save_keeps() -> anyhow::Result<()> {
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
    assert!(h.runtime.revert(saved.clone()));
    h.runtime.configure(ValidConfig::assume(Config {
        theme: Theme::Iris,
        ..saved.clone()
    }));
    assert!(
        still_pending(h.observe(|snapshot| !snapshot.show || snapshot.phase != Phase::Recording))
            .await,
        "A theme-only save after a waiting rollback ended the Show"
    );
    finish_show(&h);
    h.transcribe_next_as("look here").await?;
    receive(&h.published).await?;
    h.observe(|snapshot| !snapshot.phase.is_active()).await?;
    control(&h, Command::Status).await?;
    assert!(
        h.snapshot().message.contains("Show copied"),
        "The rollback that waited out a Show replaced the Show's result"
    );
    h.start().await?;
    finish_in(&mut h, &saved.language).await?;
    h.close().await
}
