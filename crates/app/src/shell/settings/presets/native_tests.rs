use anyhow::{Context as _, ensure};
use gpui::{AsyncApp, KeyDownEvent, KeyUpEvent, Keystroke, Task, WindowHandle};

use super::{
    super::{EngineFile, EngineWork, native_interaction::draw, vocabulary},
    *,
};
use crate::{
    config::Config,
    recognition_terms::Terms,
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
        view.edit_preset(Edit::Undo, window, cx);
        ensure!(
            view.config.presets.iter().map(Preset::name).eq(["Coding", "Other"]),
            "Undo did not put a deleted preset back in its place"
        );
        view.edit_preset(Edit::Undo, window, cx);
        ensure!(
            view.config.presets.len() == 2,
            "A second Undo restored the deleted preset twice"
        );
        view.edit_preset(Edit::Delete(0), window, cx);
        view.edit_preset(Edit::Use(0), window, cx);
        ensure!(
            view.config.language == "en",
            "The surviving preset was not usable"
        );
        let mut incomplete = serde_json::to_value(Preset::capture("Earlier", &view.config)?)?;
        incomplete
            .as_object_mut()
            .context("A preset serializes as an object")?
            .insert("model".into(), "".into());
        view.config.presets.push(serde_json::from_value(incomplete)?);
        let before = view.config.clone();
        view.edit_preset(Edit::Use(1), window, cx);
        ensure!(
            view.config == before
                && matches!(&view.notice, Some(Notice::Problem(text)) if text.contains("Choose an engine and model")),
            "Using a preset saved without a model changed dictation or did not say what to fix"
        );
        view.config.presets.pop();
        view.notice = None;
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
