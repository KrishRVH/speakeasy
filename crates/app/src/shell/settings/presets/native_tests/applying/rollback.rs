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

fn reset_view(
    settings: WindowHandle<Settings>,
    path: &Path,
    saved: &Config,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    settings.update(cx, |view, _, cx| {
        view.config = saved.clone();
        view.saves = SaveQueue::new(path.to_owned(), saved.clone());
        view.notice = None;
        cx.notify();
    })
}

fn saved_use(cx: &AsyncApp) -> Config {
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, _| {
            if let Some(used) = services.admitted.take() {
                services.config = used;
            }
            services.config.clone()
        })
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
        view.land_overtaken(written(admitted.into_inner()), saved_at, cx);
        view.finish_save(SaveOutcome::Failed(FAILED.into()), saved_at, cx);
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
async fn converged(
    cx: &mut AsyncApp,
    failure: &'static str,
    binding: &Config,
) -> anyhow::Result<()> {
    until(cx, failure, |cx| {
        Ok(cx.update(|cx| {
            let services = cx.global::<Services>();
            services.is_running()
                && services
                    .launched()
                    .is_some_and(|launched| launched.change_to(binding) < Change::Desktop)
        }))
    })
    .await
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
async fn rebound_then_failed_during_show(
    settings: WindowHandle<Settings>,
    library: &Config,
    later: Option<&Config>,
    cx: &mut AsyncApp,
) -> anyhow::Result<bool> {
    let french = Config {
        language: "fr".into(),
        ..library.clone()
    };
    let admitted = use_paused(library, french, cx).await?;
    let rebound = rebind(&admitted);
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.follow_draft(&rebound);
            services.resume(cx);
        });
    });
    let shown = async {
        let input = show_after_resume(cx).await?;
        settings.update(cx, |view, _, cx| {
            view.finish_save(SaveOutcome::Failed(FAILED.into()), saved_at, cx);
            if let Some(later) = later {
                view.finish_save(SaveOutcome::Saved(written(later.clone())), saved_at, cx);
            }
        })?;
        let kept = still_showing(cx).await;
        input.deliver(Input::Cancel);
        input.deliver(Input::Release);
        converged(
            cx,
            "The owner kept the shortcut whose save failed",
            later.unwrap_or(library),
        )
        .await?;
        anyhow::Ok(kept)
    }
    .await;
    retire(library, cx).await?;
    settings.update(cx, |view, _, cx| {
        view.config = library.clone();
        view.notice = None;
        cx.notify();
    })?;
    shown
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
pub(super) async fn rolled_back_rebinding(
    settings: WindowHandle<Settings>,
    library: &Config,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let kept = rebound_then_failed_during_show(settings, library, None, cx).await?;
    ensure!(
        kept,
        "Rolling back a failed shortcut save restarted dictation during a Show"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
pub(super) async fn rebound_then_failed(
    settings: WindowHandle<Settings>,
    library: &Config,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let theme = other_theme(library);
    let kept = rebound_then_failed_during_show(settings, library, Some(&theme), cx).await?;
    ensure!(
        kept,
        "A theme save after a failed shortcut save restarted dictation during a Show"
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
    used.recv()
        .await?
        .map(Applied::load)
        .map_err(anyhow::Error::msg)?;
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
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.config = saved.clone();
            services.restore_owner(cx);
        });
        crate::shell::settings::present_appearance(cx);
    });
    reset_view(settings, path, saved, cx)
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
        view.finish_save(SaveOutcome::Failed(FAILED.into()), saved_at, cx);
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
pub(super) async fn overtaken(
    settings: WindowHandle<Settings>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let (saved, input) = show_on_french(cx).await?;
    let french = saved_use(cx);
    let german = Config {
        language: "de".into(),
        ..other_theme(&french)
    };
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    settings.update(cx, |view, _, cx| {
        view.land_overtaken(written(german), saved_at, cx);
        view.finish_save(SaveOutcome::Failed(FAILED.into()), saved_at, cx);
    })?;
    let kept = still_showing(cx).await;
    end_show(settings, path, &saved, &input, cx).await?;
    ensure!(
        kept,
        "A save that a newer queued one overtook ended the Show instead of waiting"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
pub(super) async fn overtaken_rebinding(
    settings: WindowHandle<Settings>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    cx.update(|cx| cx.update_global::<Services, _>(|services, _| services.launch = Launch::Fake));
    let (saved, input) = show_on_french(cx).await?;
    let rebound = rebind(&saved_use(cx));
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    settings.update(cx, |view, _, cx| {
        view.land_overtaken(written(rebound.clone()), saved_at, cx);
        view.finish_save(
            SaveOutcome::Saved(written(other_theme(&rebound))),
            saved_at,
            cx,
        );
    })?;
    let kept = still_showing(cx).await;
    input.deliver(Input::Cancel);
    input.deliver(Input::Release);
    let converged = converged(
        cx,
        "The owner never took the shortcut the overtaken save changed",
        &rebound,
    )
    .await;
    relaunch(&saved, cx).await?;
    reset_view(settings, path, &saved, cx)?;
    ensure!(
        kept,
        "A shortcut save that a newer queued one overtook restarted dictation during a Show"
    );
    converged
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
pub(super) async fn printed_after_failure(
    settings: WindowHandle<Settings>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let saved = cx.update(|cx| cx.global::<Services>().config.clone());
    reset_view(settings, path, &saved, cx)?;
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    settings.update(cx, |view, window, cx| {
        view.config.print_to_scripts = !saved.print_to_scripts;
        view.edit_preset(Edit::Use(0), window, cx);
        view.finish_save(SaveOutcome::Failed(FAILED.into()), saved_at, cx);
    })?;
    until(cx, "Use did not finish", |cx| {
        settings.read_with(cx, |view, _| view.using.is_none())
    })
    .await?;
    settle(cx).await;
    let (call, receipt) = Call::new(Command::Capture { style: None });
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.control(call, cx)));
    let reply = receipt.recv().await?;
    if let Some(session) = reply.session.clone() {
        let (call, cancelled) = Call::new(Command::Cancel { session });
        cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.control(call, cx)));
        cancelled.recv().await?;
    }
    until(cx, "The capture did not end", |cx| {
        Ok(!snapshot(cx).phase.is_active())
    })
    .await?;
    reset_view(settings, path, &saved, cx)?;
    ensure!(
        !saved.print_to_scripts,
        "The fixture already lets scripts read dictation"
    );
    ensure!(
        reply.result == Disposition::Refused,
        "A Use that started on a draft whose save failed let scripts read dictation"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
pub(super) async fn older_theme_after_use(
    settings: WindowHandle<Settings>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let (saved, input) = show_on_french(cx).await?;
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    let older = other_theme(&saved);
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.adopt_saved(
                &older,
                ValidConfig::assume(older.clone()),
                saved_at,
                Landing::Saved,
                cx,
            )
        })
    })?;
    let kept = still_showing(cx).await;
    end_show(settings, path, &saved, &input, cx).await?;
    ensure!(
        kept,
        "A theme save written before a Use landed after it and ended the Show started on the preset"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, the owned Settings window, and a fake owner on GPUI's UI thread"
)]
pub(super) async fn choice_after_overtaken_save(
    settings: WindowHandle<Settings>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let saved = cx.update(|cx| cx.global::<Services>().config.clone());
    reset_view(settings, path, &saved, cx)?;
    let french = Config {
        language: "fr".into(),
        ..saved.clone()
    };
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    settings.update(cx, |view, _, cx| {
        view.land_overtaken(written(french), saved_at, cx);
    })?;
    let (call, chosen) = Call::new(Command::Preset {
        name: "Saved".into(),
    });
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.control(call, cx)));
    let chosen = chosen.recv().await?;
    settings.update(cx, |view, _, cx| {
        view.finish_save(SaveOutcome::Failed(FAILED.into()), saved_at, cx);
    })?;
    settle(cx).await;
    let named = preset_name(cx);
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.config = saved.clone();
            services.restore_owner(cx);
        });
    });
    reset_view(settings, path, &saved, cx)?;
    ensure!(
        chosen.result == Disposition::Ok && named.as_deref() == Some("Saved"),
        "A save written before a command preset erased it when a later save failed"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
pub(super) async fn fired_restart(cx: &mut AsyncApp) -> anyhow::Result<()> {
    let saved = cx.update(|cx| {
        cx.update_global::<Services, _>(|services, _| {
            services.launch = Launch::Fake;
            services.config.clone()
        })
    });
    let rebound = rebind(&saved);
    let stopped = cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.config = rebound.clone();
            services.restart_when_idle(cx);
            let waited = std::time::Instant::now();
            while services.is_running() && waited.elapsed() < PATIENCE {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let stopped = !services.is_running();
            services.restart_when_idle(cx);
            stopped
        })
    });
    let restarted = converged(cx, "A restart the owner signaled was lost", &rebound).await;
    relaunch(&saved, cx).await?;
    ensure!(stopped, "The idle owner did not stop for its restart");
    restarted
}
