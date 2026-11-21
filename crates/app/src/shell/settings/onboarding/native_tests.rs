use std::path::PathBuf;

use anyhow::ensure;
use gpui::{AsyncApp, WindowHandle};

use super::{Action, Step};
use crate::shell::{
    LaunchMode, Services,
    settings::{
        Action as SettingsAction, SaveQueue, Settings,
        native_interaction::{draw, key},
    },
};

pub(in crate::shell::settings) fn exercise(
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
        let path = cx.global::<Services>().path.clone();
        view.config.model = PathBuf::new();
        view.saves = SaveQueue::new(path.clone(), view.config.clone());
        cx.global_mut::<Services>().mode = LaunchMode::Live;
        let primary = view.onboarding_primary(Step::Prepare, cx);
        let next = view.onboarding_next(Step::Prepare, cx);
        view.config.clone_from(&original);
        view.saves = SaveQueue::new(path, original.clone());
        let failed = view.onboarding_primary(Step::Prepare, cx);
        let failed_next = view.onboarding_next(Step::Prepare, cx);
        cx.global_mut::<Services>().checked = true;
        let checked_next = view.onboarding_next(Step::Prepare, cx);
        cx.global_mut::<Services>().checked = false;
        cx.global_mut::<Services>().mode = LaunchMode::DemoTray;
        ensure!(
            primary.1 == Some(SettingsAction::Setup) && next == Step::Prepare,
            "An engine without a model skipped automatic setup"
        );
        ensure!(
            failed.1 == Some(SettingsAction::Setup) && failed_next == Step::Prepare,
            "Setup called speech ready although its saved settings never passed their check"
        );
        ensure!(
            checked_next == Step::Permissions,
            "Setup held back checked speech settings"
        );
        anyhow::Ok(())
    })??;
    draw(settings, cx)?;
    settings.update(cx, |view, window, cx| {
        view.act(SettingsAction::Onboarding(Action::Next), window, cx);
        ensure!(
            view.onboarding == Some(Step::Permissions),
            "Setup did not reach permissions"
        );
        anyhow::Ok(())
    })??;
    draw(settings, cx)?;
    settings.update(cx, |view, window, cx| {
        view.act(SettingsAction::Onboarding(Action::Next), window, cx);
        ensure!(
            view.onboarding == Some(Step::Practice),
            "Permissions did not reach practice"
        );
        ensure!(
            view.practice.live.is_none(),
            "Demo started a real practice session"
        );
        view.practice.text = zeroize::Zeroizing::new(" \n\t".into());
        view.act(SettingsAction::Onboarding(Action::Next), window, cx);
        ensure!(
            view.onboarding == Some(Step::Practice),
            "Empty recognition claimed success"
        );
        window.blur(cx);
        anyhow::Ok(())
    })??;
    draw(settings, cx)?;
    settings.update(cx, |_, window, cx| window.focus_prev(cx))?;
    key(settings, "enter", cx)?;
    ensure!(
        settings.read_with(cx, |view, _| !view.practice.text.trim().is_empty())?,
        "Demo practice produced no simulated result"
    );
    draw(settings, cx)?;
    key(settings, "enter", cx)?;
    settings.update(cx, |view, _, _| {
        ensure!(
            view.onboarding == Some(Step::Ready),
            "Recognition did not unlock completion from the focused primary control"
        );
        ensure!(
            view.practice.text.is_empty(),
            "Completion retained private text"
        );
        ensure!(
            view.practice.live.is_none(),
            "Completion retained practice ownership"
        );
        anyhow::Ok(())
    })??;
    draw(settings, cx)?;
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
