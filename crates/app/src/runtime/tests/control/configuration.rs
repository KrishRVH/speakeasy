use speakeasy_platform::{MacShortcut, WindowsShortcut};

use super::*;
use crate::config::Change;

async fn finish_in(h: &mut Harness, language: &str) -> anyhow::Result<()> {
    h.input(Input::Toggle);
    let (heard, reply) = receive(&h.jobs).await?;
    assert_eq!(heard, language);
    answer(reply, "Fixture.")?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await
}

fn rebind(config: &Config) -> Config {
    let mut linux = config.linux.clone();
    linux.external_shortcut = !linux.external_shortcut;
    Config {
        linux,
        mac_shortcut: if config.mac_shortcut == MacShortcut::F13 {
            MacShortcut::Function
        } else {
            MacShortcut::F13
        },
        windows_shortcut: if config.windows_shortcut == WindowsShortcut::F13 {
            WindowsShortcut::ControlWindows
        } else {
            WindowsShortcut::F13
        },
        ..config.clone()
    }
}

#[tokio::test(start_paused = true)]
async fn a_save_that_changes_only_the_shortcut_keeps_the_session() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.ready().await?;
    h.start().await?;
    let rebound = rebind(&h.runtime.baseline);
    assert_eq!(h.runtime.baseline.change_to(&rebound), Change::Desktop);
    h.runtime.configure(ValidConfig::assume(rebound));
    assert!(
        still_pending(h.observe(|snapshot| snapshot.phase != Phase::Recording)).await,
        "A save that changed only the shortcut ended the recording"
    );
    h.finish_as("Fixture.").await?;
    receive(&h.pasted).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_press_sent_after_a_save_records_with_the_saved_settings() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.ready().await?;
    h.runtime.configure(ValidConfig::assume(Config {
        language: "fr".into(),
        ..h.runtime.baseline.clone()
    }));
    h.input(Input::Toggle);
    h.observe(|snapshot| matches!(snapshot.phase, Phase::Recording | Phase::Cancelled))
        .await?;
    assert_eq!(
        h.snapshot().phase,
        Phase::Recording,
        "A press sent after a save started on the settings before it"
    );
    receive(&h.captures).await?;
    finish_in(&mut h, "fr").await?;
    h.close().await
}

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
