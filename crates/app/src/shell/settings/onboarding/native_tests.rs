use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::ensure;
use gpui::{AsyncApp, BorrowAppContext as _, WindowHandle};
use speakeasy_core::machine::Phase;

use super::{Action, Step};
use crate::{
    config::Config,
    shell::{
        LaunchMode, Services,
        settings::{
            Action as SettingsAction, SaveQueue, Settings,
            native_interaction::{draw, key},
        },
    },
};

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance waits on GPUI's UI-thread executor"
)]
async fn failed_check(cx: &AsyncApp) -> bool {
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(5) {
        if cx.update(|cx| cx.global::<Services>().output.borrow().phase == Phase::Error) {
            return true;
        }
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    }
    false
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance waits on GPUI's UI-thread executor"
)]
pub(in crate::shell::settings) async fn setup_rechecks_unchanged_settings(
    settings: WindowHandle<Settings>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let (original, before) = settings.update(cx, |view, _, cx| {
        let original = view.config.clone();
        let missing = Config {
            engine_executable: directory.path().join("engine"),
            model: directory.path().join("model.gguf"),
            ..original.clone()
        };
        let path = cx.global::<Services>().path.clone();
        let services = cx.global_mut::<Services>();
        let before = (
            services.config.clone(),
            services.output.borrow().clone(),
            services.speech_checked,
        );
        services.mode = LaunchMode::Live;
        services.config.clone_from(&missing);
        services.speech_checked = true;
        services
            .output
            .send_modify(|snapshot| snapshot.phase = Phase::Idle);
        view.saves = SaveQueue::new(path, missing.clone());
        view.config = missing;
        cx.update_global::<Services, _>(|services, cx| services.resume(cx));
        (original, before)
    })?;
    let resumed = failed_check(cx).await;
    let unready = settings.update(cx, |view, _, cx| {
        let unready = !cx.global::<Services>().speech_checked
            && view.onboarding_next(Step::Prepare, cx) == Step::Prepare;
        cx.global::<Services>()
            .output
            .send_modify(|snapshot| snapshot.phase = Phase::Idle);
        view.adopt_setup(cx);
        unready
    })?;
    let rechecked = failed_check(cx).await;
    settings.update(cx, |view, _, cx| {
        let path = cx.global::<Services>().path.clone();
        let services = cx.global_mut::<Services>();
        services.mode = LaunchMode::DemoTray;
        services.config = before.0;
        services.output.send_modify(|snapshot| *snapshot = before.1);
        services.speech_checked = before.2;
        view.saves = SaveQueue::new(path, original.clone());
        view.config = original;
    })?;
    ensure!(
        resumed && unready,
        "A failed Resume check left setup treating the saved speech settings as ready"
    );
    ensure!(
        rechecked,
        "Setup that kept the saved speech paths did not check them again"
    );
    Ok(())
}

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
        cx.global_mut::<Services>().speech_checked = true;
        let checked_next = view.onboarding_next(Step::Prepare, cx);
        cx.global_mut::<Services>().speech_checked = false;
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
        view.practice.text = " \n\t".into();
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
