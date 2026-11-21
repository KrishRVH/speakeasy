use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context as _, ensure};
use gpui::{AsyncApp, KeyDownEvent, KeyUpEvent, Keystroke, Task, WindowHandle};
use speakeasy_core::machine::Phase;

use super::{
    super::{EngineFile, EngineWork, native_interaction::draw, vocabulary},
    *,
};
use crate::{
    config::{Config, ValidConfig},
    recognition_terms::Terms,
    shell::choices::Selected,
    transcript::vocabulary::{Correction, Vocabulary},
};

pub(in crate::shell::settings) fn exercise(
    settings: WindowHandle<Settings>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let original = settings.read_with(cx, |view, _| view.config.clone())?;
    settings.update(cx, |view, window, cx| {
        view.config.language = "fr".into();
        view.config.vocabulary = vec![Correction {
            heard: "cube control".into(),
            written: "kubectl".into(),
        }];
        view.config.recognition_terms.add("Kubernetes")?;
        view.presets.enter("Coding", cx);
        view.edit_preset(Edit::Save, window, cx);
        ensure!(
            view.config.presets.len() == 1 && !view.presets.naming,
            "Saving a named preset did not store it"
        );
        ensure!(
            view.config
                .presets
                .first()
                .is_some_and(|preset| view.preset_in_use(preset)),
            "A new preset is not shown as in use"
        );
        view.presets.enter("Coding", cx);
        view.edit_preset(Edit::Save, window, cx);
        ensure!(
            view.config.presets.len() == 1
                && view.presets.naming
                && view.notice.as_ref().is_some_and(Notice::is_problem),
            "A duplicate preset name was stored or silently dropped"
        );
        view.presets.clear(cx);
        view.notice = None;
        view.config.language = "en".into();
        view.config.vocabulary.clear();
        view.config.recognition_terms = Terms::default();
        anyhow::Ok(())
    })??;
    draw(settings, cx)?;
    settings.update(cx, |view, window, cx| {
        let unmodified = view.config.clone();
        view.engine_work = EngineWork::Choosing {
            engine: view.config.engine,
            file: EngineFile::Model,
            _picker: Task::ready(()),
        };
        view.edit_preset(Edit::Use(0), window, cx);
        ensure!(
            view.config == unmodified,
            "Using a preset raced an outstanding file picker"
        );
        view.finish_choice(Ok(None), cx);
        view.config.vocabulary = vec![Correction {
            heard: "x ray".into(),
            written: "X-ray".into(),
        }];
        view.edit_correction(vocabulary::Edit::Revise(0), window, cx);
        view.edit_preset(Edit::Use(0), window, cx);
        view.edit_correction(vocabulary::Edit::Save, window, cx);
        ensure!(
            view.config.language == "fr"
                && Vocabulary::new(&view.config.vocabulary).text("Use cube control.".into())
                    == "Use kubectl."
                && view.config.recognition_terms.phrases() == ["Kubernetes"],
            "Using a preset did not apply its language, corrections, and hints, or a correction being revised overwrote them"
        );
        let before = view.config.presets.clone();
        let model = std::mem::take(&mut view.config.model);
        view.presets.enter("Incomplete", cx);
        view.edit_preset(Edit::Save, window, cx);
        view.edit_preset(Edit::Update(0), window, cx);
        ensure!(view.config.presets == before, "An incomplete draft changed saved presets");
        view.presets.clear(cx);
        view.config.model = model;

        view.config.language = "de".into();
        view.edit_preset(Edit::Update(0), window, cx);
        ensure!(
            view.config.presets.len() == 1
                && view.config.presets.first().is_some_and(|preset| {
                    preset.name() == "Coding" && preset.matches(&view.config)
                }),
            "Updating a preset renamed it or kept its old choices"
        );
        let english = Config {
            language: "en".into(),
            ..view.config.clone()
        };
        view.config
            .presets
            .push(Preset::capture("Other", &english)?);
        view.edit_preset(Edit::Delete(0), window, cx);
        let after_removal = view.config.clone();
        ensure!(
            after_removal.language == "de"
                && after_removal.presets.len() == 1
                && after_removal.presets.first().map(Preset::name) == Some("Other"),
            "Deleting a preset changed dictation choices or another preset"
        );
        for stale in [Edit::Use(1), Edit::Update(1), Edit::Delete(1)] {
            view.edit_preset(stale, window, cx);
            ensure!(
                view.config == after_removal,
                "An out-of-range preset action changed settings"
            );
        }
        view.edit_preset(Edit::Use(0), window, cx);
        ensure!(
            view.config.language == "en",
            "The surviving preset was not usable"
        );
        menu_cursor_survives_engine_work(view, window, cx)?;
        view.toggle_menu(Menu::Preset(0), window, cx);
        view.finish_reset(Ok(()), cx);
        ensure!(
            view.config.presets.is_empty() && view.menus.open.is_none(),
            "A reset left the menu of a preset it removed open"
        );
        view.config = original;
        view.notice = None;
        cx.notify();
        anyhow::Ok(())
    })??;
    Ok(())
}

fn menu_cursor_survives_engine_work(
    view: &mut Settings,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<'_, Settings>,
) -> anyhow::Result<()> {
    let enter = KeyUpEvent {
        keystroke: Keystroke::parse("enter")?,
    };
    view.config
        .presets
        .push(Preset::capture("Spare", &view.config)?);
    let kept = view.config.presets.clone();
    view.toggle_menu(Menu::Preset(1), window, cx);
    view.engine_work = EngineWork::Choosing {
        engine: view.config.engine,
        file: EngineFile::Model,
        _picker: Task::ready(()),
    };
    view.menu_release(&enter, window, cx);
    ensure!(
        view.config.presets == kept && view.menus.open == Some(Menu::Preset(1)),
        "Enter on Update during engine work acted on another menu entry"
    );
    view.menu_key(
        &KeyDownEvent {
            keystroke: Keystroke::parse("end")?,
            is_held: false,
            prefer_character_input: false,
        },
        window,
        cx,
    );
    view.finish_choice(Ok(None), cx);
    view.menu_release(&enter, window, cx);
    ensure!(
        view.config.presets.len() == 1
            && view.config.presets.first().map(Preset::name) == Some("Other"),
        "Delete chosen during engine work acted on another menu entry once it finished"
    );
    Ok(())
}

fn select(services: &mut Services, name: &str, config: &Config) {
    services.choices.selected = Some(Selected {
        name: name.into(),
        config: ValidConfig::assume(config.clone()),
    });
    services
        .output
        .send_modify(|snapshot| snapshot.preset = Some(name.into()));
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance waits on GPUI's UI-thread executor"
)]
async fn settle(cx: &AsyncApp) {
    cx.background_executor()
        .timer(Duration::from_millis(50))
        .await;
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and the owned Settings window on GPUI's UI thread"
)]
pub(in crate::shell::settings) async fn transient_choice(
    settings: WindowHandle<Settings>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let (presented, applied) = cx.update(|cx| {
        let services = cx.global::<Services>();
        (services.output.borrow().clone(), services.config.clone())
    });
    let (original, french) = settings.update(cx, |view, _, cx| {
        let original = view.config.clone();
        let saved = Config {
            model: Path::new(".").join(&original.model),
            ..original.clone()
        };
        let french = Config {
            language: "fr".into(),
            engine_executable: "french-server".into(),
            ..original.clone()
        };
        view.config.presets = vec![
            Preset::capture("Saved", &saved)?,
            Preset::capture("French", &french)?,
            Preset::capture("Twin", &original)?,
        ];
        cx.update_global::<Services, _>(|services, _| select(services, "French", &french));
        anyhow::Ok((original, french))
    })??;
    settle(cx).await;
    settings.update(cx, |view, window, cx| {
        let [saved, chosen, twin] = view.config.presets.as_slice() else {
            anyhow::bail!("The fixture presets are missing");
        };
        ensure!(
            view.preset_in_use(chosen) && !view.preset_in_use(saved) && !view.preset_in_use(twin),
            "Settings did not show only the tray's preset in use"
        );
        view.edit_preset(Edit::Use(0), window, cx);
        anyhow::Ok(())
    })??;
    settle(cx).await;
    settings.update(cx, |view, _, cx| {
        ensure!(
            cx.global::<Services>().preset_name().is_none(),
            "Using the saved preset, spelled with a relative model path, kept the tray's preset"
        );
        ensure!(
            view.presentation.preset.is_none()
                && view
                    .config
                    .presets
                    .first()
                    .is_some_and(|saved| view.preset_in_use(saved)),
            "Using the saved preset did not show it in use"
        );
        cx.update_global::<Services, _>(|services, _| select(services, "French", &french));
        anyhow::Ok(())
    })??;
    settle(cx).await;
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            let german = Config {
                language: "de".into(),
                ..services.config.clone()
            };
            let saved_at = services.config_epoch().next();
            services.adopt_saved(ValidConfig::assume(german), saved_at, cx)
        })
    })?;
    settle(cx).await;
    settings.update(cx, |view, _, cx| {
        ensure!(
            view.presentation.preset.is_none(),
            "A settings change saved while paused left its cleared preset presented"
        );
        cx.update_global::<Services, _>(|services, cx| {
            services.config = applied.clone();
            select(services, "French", &french);
            services.resume(cx);
        });
        anyhow::Ok(())
    })??;
    settings.update(cx, |view, window, cx| {
        view.edit_preset(Edit::Use(0), window, cx);
    })?;
    let start = Instant::now();
    while cx.update(|cx| cx.global::<Services>().output.borrow().phase) != Phase::Error {
        ensure!(
            start.elapsed() < Duration::from_secs(5),
            "Resume did not finish checking settings"
        );
        settle(cx).await;
    }
    let message = cx.update(|cx| cx.global::<Services>().output.borrow().message.clone());
    ensure!(
        message.contains("whisper-server") && !message.contains("french-server"),
        "Resume applied the preset that Use replaced while it was being checked"
    );
    settings
        .update(cx, |view, _, cx| {
            view.config = original;
            view.notice = None;
            cx.update_global::<Services, _>(|services, _| {
                services.config = applied;
                services
                    .output
                    .send_modify(|snapshot| *snapshot = presented);
            });
            cx.notify();
        })
        .context("The Settings window closed during the preset exercise")
}
