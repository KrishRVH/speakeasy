use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::ensure;
use speakeasy_core::machine::{ModelState, Phase};
#[cfg(target_os = "windows")]
use speakeasy_platform::WindowsShortcut;
use speakeasy_platform::{Input, InputSender, Scope};

use super::*;
use crate::{
    config::Engine,
    control::{Call, Command, Disposition},
    shell::{
        LaunchMode,
        choices::Selected,
        save::SaveQueue,
        services::{Landing, Launch},
    },
    status::ServiceState,
};

const PATIENCE: Duration = Duration::from_secs(5);

fn select(services: &mut Services, name: &str, config: &Config) -> anyhow::Result<()> {
    services.choices.selected = Some(Selected {
        preset: Preset::capture(name, config)?,
        config: ValidConfig::assume(config.clone()),
    });
    services
        .output
        .send_modify(|snapshot| snapshot.preset = Some(name.into()));
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance waits on GPUI's UI-thread executor"
)]
async fn settle(cx: &AsyncApp) {
    cx.background_executor()
        .timer(Duration::from_millis(20))
        .await;
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance polls the owned Settings window on GPUI's UI thread"
)]
async fn until(
    cx: &mut AsyncApp,
    failure: &'static str,
    mut done: impl FnMut(&mut AsyncApp) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let start = Instant::now();
    while !done(cx)? {
        ensure!(start.elapsed() < PATIENCE, failure);
        settle(cx).await;
    }
    Ok(())
}

fn preset_name(cx: &AsyncApp) -> Option<String> {
    cx.update(|cx| cx.global::<Services>().preset_name().map(str::to_owned))
}

fn snapshot(cx: &AsyncApp) -> crate::runtime::Snapshot {
    cx.update(|cx| cx.global::<Services>().output.borrow().clone())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and the owned Settings window on GPUI's UI thread"
)]
async fn using(
    settings: WindowHandle<Settings>,
    index: usize,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    settings.update(cx, |view, window, cx| {
        view.edit_preset(Edit::Use(index), window, cx);
    })?;
    until(cx, "Use did not finish", |cx| {
        settings.read_with(cx, |view, _| view.using.is_none() && view.saving.is_none())
    })
    .await
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and the owned Settings window on GPUI's UI thread"
)]
pub(in crate::shell::settings) async fn exercise(
    settings: WindowHandle<Settings>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let speech = crate::config::valid_config(directory.path())?
        .validated(&path)?
        .into_inner();
    let baseline = Config {
        language: "en".into(),
        ..speech
    };
    let french = Config {
        language: "fr".into(),
        ..baseline.clone()
    };
    let mut library = baseline.clone();
    let relative = Config {
        engine_executable: "engine".into(),
        model: "model.gguf".into(),
        ..baseline.clone()
    };
    library.presets = vec![
        Preset::capture("Saved", &baseline)?,
        Preset::capture("French", &french)?,
        Preset::capture("Relative", &relative)?,
    ];
    fs::write(&path, serde_json::to_vec_pretty(&library)?)?;
    let (presented, mode, settings_path, applied) = cx.update(|cx| {
        let services = cx.global::<Services>();
        let presented = services.output.borrow().clone();
        (
            presented,
            services.mode,
            services.path.clone(),
            services.config.clone(),
        )
    });
    let drafted = settings.update(cx, |view, _, cx| {
        let drafted = std::mem::replace(&mut view.config, library.clone());
        view.saves = SaveQueue::new(path.clone(), library.clone());
        cx.update_global::<Services, _>(|services, _| {
            services.mode = LaunchMode::Live;
            services.path.clone_from(&path);
            services.config = library.clone();
        });
        drafted
    })?;
    let exercised = async {
        paused(settings, &baseline, cx).await?;
        resolved(settings, &path, &library, cx).await?;
        resumed(&library, cx).await?;
        redrafted(settings, &path, &library, cx).await?;
        rechecked(&library, cx).await?;
        rebound(&library, cx).await?;
        rollback::rolled_back(settings, &library, cx).await?;
        cx.update(|cx| {
            cx.update_global::<Services, _>(|services, _| {
                services.config = library.clone();
                services.choices.selected = None;
                services.run_fake_owner()
            })
        })
    }
    .await;
    let running = match exercised {
        Ok(()) => running(settings, &path, cx).await,
        Err(error) => Err(error),
    };
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.pause(cx)));
    until(cx, "The fake owner did not retire", |cx| {
        Ok(cx.update(|cx| cx.global::<Services>().runtime().is_none()))
    })
    .await?;
    settings.update(cx, |view, _, cx| {
        view.config = drafted;
        view.saves = SaveQueue::new(settings_path.clone(), view.config.clone());
        view.notice = None;
        cx.update_global::<Services, _>(|services, _| {
            services.mode = mode;
            services.path = settings_path;
            services.config = applied;
            services.choices.selected = None;
            services
                .output
                .send_modify(|snapshot| *snapshot = presented);
        });
        cx.notify();
    })?;
    running
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and the owned Settings window on GPUI's UI thread"
)]
async fn paused(
    settings: WindowHandle<Settings>,
    baseline: &Config,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let broken = Config {
        language: "fr".into(),
        engine_executable: baseline.engine_executable.with_file_name("french-server"),
        ..baseline.clone()
    };
    let moved = baseline.model.with_extension("moved");
    fs::rename(&baseline.model, &moved)?;
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            select(services, "French", &broken)?;
            services.resume(cx);
            anyhow::Ok(())
        })
    })?;
    using(settings, 0, cx).await?;
    let kept = preset_name(cx);
    let refused = settings.read_with(cx, |view, _| {
        view.notice.as_ref().is_some_and(Notice::is_problem)
    })?;
    until(cx, "Resume did not finish checking settings", |cx| {
        Ok(snapshot(cx).phase == Phase::Error)
    })
    .await?;
    let checked = snapshot(cx).message;
    fs::rename(&moved, &baseline.model)?;
    ensure!(
        kept.as_deref() == Some("French") && refused,
        "Using a saved preset whose model is missing forgot the working preset"
    );
    ensure!(
        checked.contains("french-server"),
        "Use redirected the pending Resume check to the unavailable preset"
    );

    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            select(services, "French", &broken)?;
            let german = Config {
                language: "de".into(),
                ..services.config.clone()
            };
            let saved_at = services.config_epoch().next();
            services.adopt_saved(
                &german,
                ValidConfig::assume(german.clone()),
                saved_at,
                Landing::Saved,
                cx,
            )
        })
    })?;
    settle(cx).await;
    settings.update(cx, |view, _, _| {
        ensure!(
            view.presentation.preset.is_none(),
            "A settings change saved while paused left its cleared preset presented"
        );
        view.notice = None;
        anyhow::Ok(())
    })?
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and the owned Settings window on GPUI's UI thread"
)]
async fn resolved(
    settings: WindowHandle<Settings>,
    path: &Path,
    library: &Config,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let edited = Config {
        model: path.with_file_name("edited elsewhere.gguf"),
        ..library.clone()
    };
    fs::write(path, serde_json::to_vec_pretty(&edited)?)?;
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, _| {
            services.config = library.clone();
            services.speech_checked = false;
            services.choices.selected = None;
            services
                .output
                .send_modify(|snapshot| snapshot.preset = None);
        });
    });
    settings.update(cx, |view, _, cx| {
        view.config = library.clone();
        view.saves = SaveQueue::new(path.to_owned(), library.clone());
        cx.notify();
    })?;
    using(settings, 2, cx).await?;
    ensure!(
        cx.update(|cx| cx.global::<Services>().speech_checked),
        "A preset that resolved to the saved speech settings left them unchecked"
    );
    ensure!(
        Config::read(path)?.model == edited.model,
        "Using a preset that spells the saved files relatively rewrote the settings file"
    );
    settings.read_with(cx, |view, _| {
        ensure!(
            view.config
                .presets
                .get(2)
                .is_some_and(|relative| view.preset_in_use(relative)),
            "The relatively spelled preset still offered Use after it applied"
        );
        anyhow::Ok(())
    })??;

    let switched = settings.update(cx, |view, window, cx| {
        view.edit_preset(Edit::Use(1), window, cx);
        view.act(Action::Engine(other_engine(&view.config)), window, cx);
        view.config.clone()
    })?;
    until(cx, "Use did not finish", |cx| {
        settings.read_with(cx, |view, _| view.using.is_none())
    })
    .await?;
    let (kept, warned) = settings.update(cx, |view, _, cx| {
        let outcome = (
            view.config == switched,
            view.notice.take().is_some_and(|notice| notice.is_problem()),
        );
        view.config = library.clone();
        view.saves = SaveQueue::new(path.to_owned(), library.clone());
        cx.notify();
        outcome
    })?;
    ensure!(
        kept && warned && Config::read(path)?.language == edited.language,
        "A preset whose check finished after an engine switch replaced the newer draft"
    );
    fs::write(path, serde_json::to_vec_pretty(library)?)?;
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance waits on a real settings write on GPUI's UI thread"
)]
async fn redrafted(
    settings: WindowHandle<Settings>,
    path: &Path,
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
    let used = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.config = library.clone();
                services.choices.selected = None;
                services.use_preset(french, |_| true, cx)
            })
        })
        .map_err(anyhow::Error::msg)?;
    used.recv().await?.map_err(anyhow::Error::msg)?;
    settings.update(cx, |view, _, cx| {
        view.saves = SaveQueue::new(path.to_owned(), library.clone());
        view.config = german.clone();
        view.commit(cx);
    })?;
    until(cx, "The newer draft was not saved", |cx| {
        settings.read_with(cx, |view, _| view.saves.is_idle())
    })
    .await?;
    let (followed, saved) = cx.update(|cx| {
        cx.update_global::<Services, _>(|services, _| {
            let outcome = (services.admitted.is_none(), services.config == german);
            services.admitted = None;
            services.config = library.clone();
            outcome
        })
    });
    settings.update(cx, |view, _, cx| {
        view.config = library.clone();
        view.saves = SaveQueue::new(path.to_owned(), library.clone());
        view.notice = None;
        cx.notify();
    })?;
    fs::write(path, serde_json::to_vec_pretty(library)?)?;
    ensure!(
        saved && followed,
        "A draft saved after a paused Use left Resume on the used preset"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services on GPUI's UI thread"
)]
async fn use_paused(
    library: &Config,
    preset: Config,
    cx: &AsyncApp,
) -> anyhow::Result<ValidConfig> {
    let used = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.config = library.clone();
                services.choices.selected = None;
                services.launch = Launch::Fake;
                services.use_preset(preset, |_| true, cx)
            })
        })
        .map_err(anyhow::Error::msg)?;
    used.recv().await?.map_err(anyhow::Error::msg)
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives a fake owner on GPUI's UI thread"
)]
async fn show_after_resume(cx: &mut AsyncApp) -> anyhow::Result<InputSender> {
    until(cx, "Resume did not start the fake owner", |cx| {
        Ok(snapshot(cx).model == ModelState::Ready)
    })
    .await?;
    let input = cx
        .update(|cx| {
            cx.global::<Services>()
                .runtime()
                .map(|runtime| runtime.input.clone())
        })
        .context("Resume started no owner")?;
    input.deliver(Input::Press);
    until(cx, "The resumed owner did not record", |cx| {
        Ok(snapshot(cx).phase == Phase::Recording)
    })
    .await?;
    input.deliver(Input::Show(Scope::Screen));
    until(cx, "The resumed owner did not start a Show", |cx| {
        Ok(snapshot(cx).show)
    })
    .await?;
    Ok(input)
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives a fake owner on GPUI's UI thread"
)]
async fn still_showing(cx: &AsyncApp) -> bool {
    settle(cx).await;
    settle(cx).await;
    let after = snapshot(cx);
    after.show && after.phase == Phase::Recording
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance retires a fake owner on GPUI's UI thread"
)]
async fn retire(library: &Config, cx: &mut AsyncApp) -> anyhow::Result<()> {
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.launch = Launch::Desktop;
            services.pause(cx);
        });
    });
    until(cx, "The resumed owner did not retire", |cx| {
        Ok(cx.update(|cx| {
            let services = cx.global::<Services>();
            services.runtime().is_none() && services.service_state() != ServiceState::Pausing
        }))
    })
    .await?;
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, _| {
            services.config = library.clone();
            services.admitted = None;
        });
    });
    Ok(())
}

fn rebind(config: &Config) -> Config {
    let mut rebound = config.clone();
    #[cfg(target_os = "linux")]
    {
        rebound.linux.external_shortcut = !rebound.linux.external_shortcut;
    }
    #[cfg(target_os = "windows")]
    {
        rebound.windows_shortcut = if rebound.windows_shortcut == WindowsShortcut::F13 {
            WindowsShortcut::RightControl
        } else {
            WindowsShortcut::F13
        };
    }
    rebound
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
async fn resumed(library: &Config, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let french = Config {
        language: "fr".into(),
        ..library.clone()
    };
    let admitted = use_paused(library, french, cx).await?;
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.resume(cx)));
    let shown = async {
        let input = show_after_resume(cx).await?;
        cx.update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                let submitted = Config::clone(&admitted);
                services.adopt_saved(&submitted, admitted, saved_at, Landing::Saved, cx)
            })
        })?;
        let kept = still_showing(cx).await;
        input.deliver(Input::Cancel);
        anyhow::Ok(kept)
    }
    .await;
    retire(library, cx).await?;
    ensure!(
        shown?,
        "Saving the preset used while paused discarded the Show started after Resume"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
async fn rechecked(library: &Config, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let french = Config {
        language: "fr".into(),
        ..library.clone()
    };
    let german = Config {
        language: "de".into(),
        ..library.clone()
    };
    use_paused(library, french, cx).await?;
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.resume(cx);
            services.follow_draft(&german);
        });
    });
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
    started?;
    ensure!(
        launched.as_deref() == Some("de"),
        "A draft written while Resume checked a paused Use left the owner on the older preset"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
async fn rebound(library: &Config, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let french = Config {
        language: "fr".into(),
        ..library.clone()
    };
    let admitted = use_paused(library, french, cx).await?;
    let older = Config::clone(&admitted);
    let rebound = rebind(&older);
    let saved_at = cx.update(|cx| cx.global::<Services>().config_epoch());
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.follow_draft(&rebound);
            services.resume(cx);
        });
    });
    let shown = async {
        let input = show_after_resume(cx).await?;
        cx.update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.adopt_saved(&older, admitted, saved_at, Landing::Saved, cx)
            })
        })?;
        let outlasted = still_showing(cx).await;
        cx.update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.adopt_saved(
                    &rebound,
                    ValidConfig::assume(rebound.clone()),
                    saved_at,
                    Landing::Saved,
                    cx,
                )
            })
        })?;
        let kept = still_showing(cx).await;
        input.deliver(Input::Cancel);
        anyhow::Ok((outlasted, kept))
    }
    .await;
    retire(library, cx).await?;
    let (outlasted, kept) = shown?;
    ensure!(
        outlasted,
        "An older save landing after Resume discarded the Show started on the newer draft"
    );
    ensure!(
        kept,
        "The newest save's shortcut, already in use since Resume, paused the owner"
    );
    Ok(())
}

fn other_engine(config: &Config) -> Engine {
    if config.engine == Engine::Whisper {
        Engine::Nemo
    } else {
        Engine::Whisper
    }
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, a fake owner, and the owned Settings window on GPUI's UI thread"
)]
async fn stale_use(
    settings: WindowHandle<Settings>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    const UNTOUCHED: &str = "The owner has not stepped";
    let draft = settings.read_with(cx, |view, _| view.config.clone())?;
    let switched = settings.update(cx, |view, window, cx| {
        cx.global::<Services>()
            .output
            .send_modify(|snapshot| snapshot.message = UNTOUCHED.into());
        view.edit_preset(Edit::Use(1), window, cx);
        view.act(Action::Engine(other_engine(&view.config)), window, cx);
        view.config.clone()
    })?;
    until(cx, "Use did not finish", |cx| {
        settings.read_with(cx, |view, _| view.using.is_none())
    })
    .await?;
    settle(cx).await;
    let untouched = snapshot(cx).message == UNTOUCHED;
    let (kept, warned) = settings.update(cx, |view, _, _| {
        (
            view.config == switched,
            view.notice.take().is_some_and(|notice| notice.is_problem()),
        )
    })?;
    ensure!(
        untouched && kept && warned,
        "A Use overtaken while it validated still reached the owner"
    );

    let input = cx
        .update(|cx| {
            cx.global::<Services>()
                .runtime()
                .map(|runtime| runtime.input.clone())
        })
        .context("The fake owner stopped")?;
    settings.update(cx, |view, _, cx| {
        view.config = draft.clone();
        view.saves = SaveQueue::new(path.to_owned(), draft.clone());
        cx.notify();
    })?;
    let updates = cx.update(|cx| cx.global::<Services>().output.subscribe());
    let mut adopting = false;
    let mut armed = Some(input.clone());
    let observation = cx.update(|cx| {
        cx.observe_global::<Services>(move |cx| {
            if cx.global::<Services>().choices.awaits_owner() {
                adopting = true;
                return;
            }
            if !adopting {
                return;
            }
            let Some(input) = armed.take() else {
                return;
            };
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The acceptance reads the draft from Settings; a closed window already failed it"
            )]
            let _ = settings.update(cx, |view, window, cx| {
                view.act(Action::Engine(other_engine(&view.config)), window, cx);
            });
            input.deliver(Input::Press);
            let began = Instant::now();
            while updates.borrow().phase != Phase::Recording && began.elapsed() < PATIENCE {
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    });
    settings.update(cx, |view, window, cx| {
        view.edit_preset(Edit::Use(1), window, cx);
    })?;
    until(cx, "Use did not finish", |cx| {
        settings.read_with(cx, |view, _| view.using.is_none())
    })
    .await?;
    drop(observation);
    settle(cx).await;
    settle(cx).await;
    let recording = snapshot(cx).phase == Phase::Recording;
    input.deliver(Input::Cancel);
    input.deliver(Input::Release);
    until(cx, "The recording did not end", |cx| {
        Ok(!snapshot(cx).phase.is_active())
    })
    .await?;
    settings.update(cx, |view, _, cx| {
        view.config = draft.clone();
        view.saves = SaveQueue::new(path.to_owned(), draft);
        view.notice = None;
        cx.notify();
    })?;
    settle(cx).await;
    ensure!(
        recording,
        "A stale Use rolled the owner back over a recording started after it adopted the preset"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
async fn unavailable(settings: WindowHandle<Settings>, cx: &AsyncApp) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let missing = settings.read_with(cx, |view, _| Config {
        model: directory.path().join("missing.gguf"),
        ..view.config.clone()
    })?;
    let (call, acknowledging) = Call::new(Command::Preset {
        name: "French".into(),
    });
    let (tried, attempt) = std::sync::mpsc::channel();
    let mut armed = Some((missing, tried));
    let observation = cx.update(|cx| {
        cx.observe_global::<Services>(move |cx| {
            if !cx.global::<Services>().choices.awaits_owner() {
                return;
            }
            let Some((missing, tried)) = armed.take() else {
                return;
            };
            let used = cx.update_global::<Services, _>(|services, cx| {
                services.use_preset(missing, |_| true, cx)
            });
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The acceptance fails below when the attempt never arrives"
            )]
            let _ = tried.send(used);
        })
    });
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.control(call, cx)));
    let answered = acknowledging.recv().await?;
    let used = attempt
        .recv_timeout(PATIENCE)?
        .map_err(anyhow::Error::msg)?
        .recv()
        .await?;
    drop(observation);
    settle(cx).await;
    ensure!(
        answered.result == Disposition::Ok
            && used.is_err()
            && preset_name(cx).as_deref() == Some("French"),
        "An unavailable Use erased a tray preset the owner had accepted"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services, a fake owner, and the owned Settings window on GPUI's UI thread"
)]
async fn running(
    settings: WindowHandle<Settings>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    until(cx, "The fake owner did not load its model", |cx| {
        Ok(snapshot(cx).model == ModelState::Ready)
    })
    .await?;
    stale_use(settings, path, cx).await?;
    rollback::waited_out(settings, path, cx).await?;
    rollback::retained(settings, path, cx).await?;

    let overtaken = settings.update(cx, |view, window, cx| {
        let (call, receipt) = Call::new(Command::Preset {
            name: "French".into(),
        });
        cx.update_global::<Services, _>(|services, cx| services.control(call, cx));
        view.edit_preset(Edit::Use(0), window, cx);
        receipt
    })?;
    until(cx, "Use did not finish", |cx| {
        settings.read_with(cx, |view, _| view.using.is_none())
    })
    .await?;
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Use replaces the tray choice whether the owner applied it first or never saw it"
    )]
    let _ = overtaken.recv().await;
    ensure!(
        preset_name(cx).is_none() && snapshot(cx).preset.is_none(),
        "A tray preset chosen just before Use overtook it"
    );
    unavailable(settings, cx).await?;

    let (call, acknowledging) = Call::new(Command::Preset {
        name: "French".into(),
    });
    let mut armed = true;
    let observation = cx.update(|cx| {
        cx.observe_global::<Services>(move |cx| {
            if !armed || !cx.global::<Services>().choices.awaits_owner() {
                return;
            }
            armed = false;
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The acceptance reads the Use outcome from Settings; a closed window already failed it"
            )]
            let _ = settings.update(cx, |view, window, cx| {
                view.edit_preset(Edit::Use(0), window, cx);
            });
        })
    });
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.control(call, cx)));
    let answered = acknowledging.recv().await;
    until(cx, "Use did not finish", |cx| {
        settings.read_with(cx, |view, _| view.using.is_none())
    })
    .await?;
    drop(observation);
    let refused = settings.update(cx, |view, _, _| {
        view.notice.take().is_some_and(|notice| notice.is_problem())
    })?;
    ensure!(
        answered.is_ok() && !refused && preset_name(cx).is_none() && snapshot(cx).preset.is_none(),
        "A tray preset awaiting the owner's answer kept a later Use from replacing it"
    );

    let (call, chosen) = Call::new(Command::Preset {
        name: "French".into(),
    });
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.control(call, cx)));
    ensure!(
        chosen.recv().await?.result == Disposition::Ok,
        "The tray could not choose a preset"
    );
    settings.update(cx, |view, _, _| {
        let [saved, french, _] = view.config.presets.as_slice() else {
            anyhow::bail!("The fixture presets are missing");
        };
        ensure!(
            view.preset_in_use(french) && !view.preset_in_use(saved),
            "Settings did not show only the tray's preset in use"
        );
        anyhow::Ok(())
    })??;
    using(settings, 0, cx).await?;
    ensure!(
        preset_name(cx).is_none() && snapshot(cx).preset.is_none(),
        "Using the saved preset in Settings kept the tray's preset"
    );

    let entity = cx.update(|cx| settings.entity(cx))?;
    let input = cx
        .update(|cx| {
            cx.global::<Services>()
                .runtime()
                .map(|runtime| runtime.input.clone())
        })
        .context("The fake owner stopped")?;
    let updates = cx.update(|cx| cx.global::<Services>().output.subscribe());
    let (started, recorded) = std::sync::mpsc::channel();
    let mut armed = Some((input.clone(), started));
    let observation = cx.update(|cx| {
        cx.observe(&entity, move |entity, cx| {
            if entity.read(cx).saving.is_none() {
                return;
            }
            let Some((input, started)) = armed.take() else {
                return;
            };
            input.deliver(Input::Toggle);
            let began = Instant::now();
            while updates.borrow().phase != Phase::Recording && began.elapsed() < PATIENCE {
                std::thread::sleep(Duration::from_millis(5));
            }
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The acceptance reads the phase after the save; a closed receiver means it already failed"
            )]
            let _ = started.send(updates.borrow().phase);
        })
    });
    using(settings, 1, cx).await?;
    drop(observation);
    let held = recorded.try_recv().ok();
    let survived = snapshot(cx).phase;
    input.deliver(Input::Cancel);
    ensure!(
        held == Some(Phase::Recording),
        "The fake owner did not record while the preset was being saved"
    );
    ensure!(
        survived == Phase::Recording,
        "Saving the used preset abandoned the session that started meanwhile"
    );
    let stored = Config::read(path)?;
    let adopted = cx.update(|cx| cx.global::<Services>().config.language.clone());
    ensure!(
        stored.language == "fr" && adopted == "fr",
        "Settings did not save the preset it used"
    );

    until(cx, "The cancelled session did not end", |cx| {
        Ok(!snapshot(cx).phase.is_active())
    })
    .await?;
    let (call, chosen) = Call::new(Command::Preset {
        name: "Saved".into(),
    });
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.control(call, cx)));
    ensure!(
        chosen.recv().await?.result == Disposition::Ok,
        "The tray could not choose the saved preset"
    );
    settings.update(cx, |view, window, cx| {
        view.edit_preset(Edit::Update(0), window, cx);
    })?;
    until(cx, "Updating the preset did not save", |cx| {
        settings.read_with(cx, |view, _| view.saving.is_none())
    })
    .await?;
    let chosen = preset_name(cx);
    settings.read_with(cx, |view, _| {
        ensure!(
            chosen.as_deref() == Some("Saved")
                && view
                    .config
                    .presets
                    .first()
                    .is_some_and(|saved| !view.preset_in_use(saved)),
            "A preset updated to other settings still showed the tray's choice in use"
        );
        anyhow::Ok(())
    })??;

    let directory = path.parent().context("The settings file has a directory")?;
    let writable = (
        fs::metadata(directory)?.permissions(),
        fs::metadata(path)?.permissions(),
    );
    for locked in [directory, path] {
        let mut permissions = fs::metadata(locked)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(locked, permissions)?;
    }
    let failed = using(settings, 2, cx).await;
    fs::set_permissions(path, writable.1)?;
    fs::set_permissions(directory, writable.0)?;
    failed?;
    let reported = settings.read_with(cx, |view, _| {
        view.notice.as_ref().is_some_and(Notice::is_problem)
    })?;
    let (call, chosen) = Call::new(Command::Preset {
        name: "French".into(),
    });
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.control(call, cx)));
    ensure!(
        reported && chosen.recv().await?.result == Disposition::Ok,
        "A preset whose save failed kept dictation off the saved settings"
    );

    let stopped = cx.update(|cx| {
        cx.global::<Services>()
            .runtime()
            .map(crate::runtime::Runtime::request_stop)
    });
    ensure!(stopped.is_some(), "The fake owner was already gone");
    let mut english = cx.update(|cx| cx.global::<Services>().config.clone());
    english.language = "en".into();
    let used = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.use_preset(english, |_| true, cx)
            })
        })
        .map_err(anyhow::Error::msg)?;
    ensure!(
        used.recv().await?.is_ok(),
        "Use refused a preset because the owner had stopped on its own"
    );
    Ok(())
}

mod rollback;
