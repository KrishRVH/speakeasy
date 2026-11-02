use anyhow::ensure;
use gpui::{AsyncApp, WindowHandle};

use super::{Action, Step};
use crate::shell::settings::{Action as SettingsAction, Settings, native_interaction::settle};

#[expect(
    clippy::future_not_send,
    reason = "Owned-window interaction runs on GPUI's application thread"
)]
pub(in crate::shell::settings) async fn exercise(
    settings: WindowHandle<Settings>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let original = settings.read_with(cx, |view, _| view.config.clone())?;
    settings.update(cx, |view, window, cx| {
        view.act(SettingsAction::Onboarding(Action::Replay), window, cx);
        ensure!(
            view.onboarding == Some(Step::Prepare),
            "Replay did not open setup"
        );
        anyhow::Ok(())
    })??;
    settle(cx).await;
    settings.update(cx, |view, window, cx| {
        view.act(SettingsAction::Onboarding(Action::Next), window, cx);
        ensure!(
            view.onboarding == Some(Step::Permissions),
            "Setup did not reach permissions"
        );
        anyhow::Ok(())
    })??;
    settle(cx).await;
    settings.update(cx, |view, window, cx| {
        view.act(SettingsAction::Onboarding(Action::Next), window, cx);
        ensure!(
            view.onboarding == Some(Step::Practice),
            "Permissions did not reach practice"
        );
        ensure!(
            view.practice.task.is_none(),
            "Demo started a real practice session"
        );
        view.practice.text = zeroize::Zeroizing::new(" \n\t".into());
        view.act(SettingsAction::Onboarding(Action::Next), window, cx);
        ensure!(
            view.onboarding == Some(Step::Practice),
            "Empty recognition claimed success"
        );
        view.act(SettingsAction::Practice, window, cx);
        ensure!(
            !view.practice.text.is_empty(),
            "Demo practice produced no simulated result"
        );
        anyhow::Ok(())
    })??;
    settle(cx).await;
    settings.update(cx, |view, window, cx| {
        view.act(SettingsAction::Onboarding(Action::Next), window, cx);
        ensure!(
            view.onboarding == Some(Step::Ready),
            "Recognition did not unlock completion"
        );
        ensure!(
            view.practice.text.is_empty(),
            "Completion retained private text"
        );
        ensure!(
            view.practice.task.is_none(),
            "Completion retained practice ownership"
        );
        anyhow::Ok(())
    })??;
    settle(cx).await;
    settings.update(cx, |view, window, cx| {
        view.act(SettingsAction::Onboarding(Action::Finish), window, cx);
        ensure!(
            view.onboarding.is_none(),
            "Finish did not return to Settings"
        );
        ensure!(view.config == original, "The tour changed user settings");
        view.act(SettingsAction::Onboarding(Action::Replay), window, cx);
        view.act(SettingsAction::Onboarding(Action::Finish), window, cx);
        ensure!(
            view.onboarding.is_none(),
            "Skip waited for model installation"
        );
        ensure!(
            view.onboarding_completion.is_none(),
            "Demo wrote an onboarding marker"
        );
        view.config.threads = 17;
        view.act(SettingsAction::Onboarding(Action::Replay), window, cx);
        ensure!(
            view.onboarding == Some(Step::Prepare) && view.config.threads == 17,
            "Replay refused or discarded current settings"
        );
        view.act(SettingsAction::Onboarding(Action::Finish), window, cx);
        view.config = original;
        view.notice = None;
        cx.notify();
        anyhow::Ok(())
    })??;
    Ok(())
}
