use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context as _, ensure};
use speakeasy_core::machine::{ModelState, Phase};
use speakeasy_platform::Input;

use super::*;
use crate::{
    control::{Call, Command, Disposition},
    shell::{LaunchMode, choices::Selected, save::SaveQueue},
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
        cx.update(|cx| {
            cx.update_global::<Services, _>(|services, _| {
                services.config = library.clone();
                services.choices.selected = None;
                services.run_fake_owner()
            })
        })
        .map(|reports| (reports, ()))
    }
    .await;
    let running = match exercised {
        Ok((reports, ())) => {
            let ran = running(settings, &path, cx).await;
            drop(reports);
            ran
        },
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
            services.adopt_saved(ValidConfig::assume(german), saved_at, cx)
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
    fs::write(path, serde_json::to_vec_pretty(library)?)?;
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
    let overtook = overtaken
        .recv()
        .await
        .is_ok_and(|reply| reply.result == Disposition::Ok);
    ensure!(
        !overtook && preset_name(cx).is_none() && snapshot(cx).preset.is_none(),
        "A tray preset chosen just before Use overtook it"
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
    Ok(())
}
