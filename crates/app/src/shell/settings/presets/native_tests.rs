use anyhow::ensure;
use gpui::{AsyncApp, Task, WindowHandle};

use super::{super::vocabulary, *};
use crate::{
    config::Config,
    recognition_terms::Terms,
    transcript::vocabulary::{Correction, Vocabulary},
};

#[expect(
    clippy::future_not_send,
    reason = "Owned Settings actions use GPUI's UI-thread context"
)]
pub(in crate::shell::settings) async fn exercise(
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
                .is_some_and(|preset| preset.matches(&view.config)),
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
    cx.background_executor()
        .timer(std::time::Duration::from_millis(100))
        .await;
    settings.update(cx, |view, window, cx| {
        let unmodified = view.config.clone();
        view.file_picker = Some(Task::ready(()));
        view.edit_preset(Edit::Use(0), window, cx);
        ensure!(
            view.config == unmodified,
            "Using a preset raced an outstanding file picker"
        );
        view.finish_choice(Ok(None), super::super::EngineFile::Model, cx);
        view.config.vocabulary = vec![Correction {
            heard: "x ray".into(),
            written: "X-ray".into(),
        }];
        view.edit_correction(vocabulary::Edit::Revise(0), window, cx);
        view.edit_preset(Edit::Use(0), window, cx);
        view.edit_correction(vocabulary::Edit::Save, window, cx);
        ensure!(
            view.config.language == "fr"
                && Vocabulary::new(&view.config.vocabulary).text("Use cube control.")
                    == "Use kubectl."
                && view.config.recognition_terms.phrases() == ["Kubernetes"],
            "Using a preset did not apply its language, corrections, and hints, or a correction being revised overwrote them"
        );
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
            .push(Preset::capture("Other".into(), &english)?);
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
