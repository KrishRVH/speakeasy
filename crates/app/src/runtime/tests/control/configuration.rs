use speakeasy_platform::{MacShortcut, Toggles, WindowsShortcut};

use super::*;
use crate::config::Change;

fn rebind(config: &Config) -> Config {
    let mut linux = config.linux.clone();
    linux.external_shortcut = !linux.external_shortcut;
    Config {
        linux,
        mac_shortcut: if config.mac_shortcut == MacShortcut::F13 {
            MacShortcut::FUNCTION
        } else {
            MacShortcut::F13
        },
        windows_shortcut: if config.windows_shortcut == WindowsShortcut::F13 {
            WindowsShortcut::CONTROL_WINDOWS
        } else {
            WindowsShortcut::F13
        },
        mac_toggles: Toggles {
            hands_free: config.mac_toggles.hands_free.xor(Some(MacShortcut::RIGHT_OPTION)),
            ..config.mac_toggles
        },
        windows_toggles: Toggles {
            hands_free: config
                .windows_toggles
                .hands_free
                .xor(Some(WindowsShortcut::RIGHT_CONTROL)),
            ..config.windows_toggles
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
        "A save that changed only shortcuts ended the recording"
    );
    h.finish_as("Fixture.").await?;
    receive(&h.pasted).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_restart_waits_for_the_session_then_stops_the_owner() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.ready().await?;
    h.start().await?;
    let (restart, restarted) = async_channel::bounded(1);
    h.runtime.restart_when_idle(restart);
    let exited = h.exited();
    assert!(
        still_pending(restarted.recv()).await,
        "A restart stopped the owner during a session"
    );
    h.finish_as("Fixture.").await?;
    receive(&h.pasted).await?;
    receive(&restarted).await?;
    timeout(PATIENCE, exited).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_newer_save_withdraws_a_waiting_restart() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.ready().await?;
    h.start().await?;
    let (restart, restarted) = async_channel::bounded(1);
    h.runtime.restart_when_idle(restart);
    h.runtime.configure(ValidConfig::assume(Config {
        language: "fr".into(),
        ..h.runtime.baseline.clone()
    }));
    assert!(
        timeout(PATIENCE, restarted.recv()).await?.is_err(),
        "A save sent after a restart request left the owner waiting to restart"
    );
    h.start().await?;
    h.finish_as("Fixture.").await?;
    receive(&h.pasted).await?;
    h.close().await
}
