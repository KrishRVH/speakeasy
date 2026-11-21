use speakeasy_platform::{Input, Scope};

use super::*;
use crate::{
    shell::save::{SaveOutcome, SavedConfig},
    theme::Theme,
};

const FAILED: &str = "Fixture write failed.";

fn other_theme(config: &Config) -> Config {
    Config {
        theme: if config.theme == Theme::Iris {
            Theme::Emerald
        } else {
            Theme::Iris
        },
        ..config.clone()
    }
}

fn written(config: Config) -> Box<SavedConfig> {
    Box::new(SavedConfig {
        submitted: config.clone(),
        validated: ValidConfig::assume(config),
    })
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
pub(super) async fn rolled_back(
    settings: WindowHandle<Settings>,
    library: &Config,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let french = Config {
        language: "fr".into(),
        ..library.clone()
    };
    let german = Config {
        language: "de".into(),
        ..library.clone()
    };
    let admitted = use_paused(library, french, cx).await?;
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    settings.update(cx, |view, _, cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.resume(cx);
            services.follow_draft(&german);
        });
        view.finish_save(
            SaveOutcome::Failed {
                error: FAILED.into(),
                kept: Some(written(admitted.into_inner())),
            },
            saved_at,
            cx,
        );
    })?;
    let started = until(cx, "Resume did not start the fake owner", |cx| {
        Ok(snapshot(cx).model == ModelState::Ready)
    })
    .await;
    let launched = cx.update(|cx| {
        cx.global::<Services>()
            .launched()
            .map(|config| config.language.clone())
    });
    retire(library, cx).await?;
    settings.update(cx, |view, _, cx| {
        view.config = library.clone();
        view.notice = None;
        cx.notify();
    })?;
    started?;
    ensure!(
        launched.as_deref() == Some("fr"),
        "Resume launched a draft whose save failed instead of the saved settings"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
async fn show_on_french(cx: &mut AsyncApp) -> anyhow::Result<(Config, InputSender)> {
    let (saved, input) = cx.update(|cx| {
        let services = cx.global::<Services>();
        (
            services.config.clone(),
            services.runtime().map(|runtime| runtime.input.clone()),
        )
    });
    let input = input.context("The fake owner stopped")?;
    let french = Config {
        language: "fr".into(),
        ..saved.clone()
    };
    let used = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.use_preset(french, |_| true, cx)
            })
        })
        .map_err(anyhow::Error::msg)?;
    used.recv().await?.map_err(anyhow::Error::msg)?;
    input.deliver(Input::Press);
    until(cx, "The fake owner did not record", |cx| {
        Ok(snapshot(cx).phase == Phase::Recording)
    })
    .await?;
    input.deliver(Input::Show(Scope::Screen));
    until(cx, "The fake owner did not start a Show", |cx| {
        Ok(snapshot(cx).show)
    })
    .await?;
    Ok((saved, input))
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
async fn end_show(
    settings: WindowHandle<Settings>,
    path: &Path,
    saved: &Config,
    input: &InputSender,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    input.deliver(Input::Cancel);
    input.deliver(Input::Release);
    until(cx, "The Show did not end", |cx| {
        Ok(!snapshot(cx).phase.is_active())
    })
    .await?;
    settings.update(cx, |view, _, cx| {
        cx.update_global::<Services, _>(|services, _| services.config = saved.clone());
        crate::shell::settings::present_appearance(cx);
        view.config = saved.clone();
        view.saves = SaveQueue::new(path.to_owned(), saved.clone());
        view.notice = None;
        cx.notify();
    })
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
pub(super) async fn waited_out(
    settings: WindowHandle<Settings>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let (saved, input) = show_on_french(cx).await?;
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    settings.update(cx, |view, _, cx| {
        view.finish_save(
            SaveOutcome::Failed {
                error: FAILED.into(),
                kept: None,
            },
            saved_at,
            cx,
        );
        view.finish_save(
            SaveOutcome::Saved(written(other_theme(&saved))),
            saved_at,
            cx,
        );
    })?;
    let kept = still_showing(cx).await;
    end_show(settings, path, &saved, &input, cx).await?;
    ensure!(
        kept,
        "A theme save after a failed preset save ended the Show the owner kept"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
pub(super) async fn retained(
    settings: WindowHandle<Settings>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let (saved, input) = show_on_french(cx).await?;
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    settings.update(cx, |view, _, cx| {
        view.finish_save(
            SaveOutcome::Failed {
                error: FAILED.into(),
                kept: Some(written(other_theme(&saved))),
            },
            saved_at,
            cx,
        );
    })?;
    let kept = still_showing(cx).await;
    end_show(settings, path, &saved, &input, cx).await?;
    ensure!(
        kept,
        "Settings applied the save kept before a failed one over the Show instead of waiting"
    );
    Ok(())
}
