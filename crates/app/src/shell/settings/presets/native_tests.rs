use anyhow::ensure;
use gpui::{AsyncApp, Task, Timer, WindowHandle};

use super::*;
use crate::{recognition_terms::Terms, transcript::vocabulary::Vocabulary};

#[expect(
    clippy::future_not_send,
    reason = "Owned Settings actions use GPUI's UI-thread context"
)]
pub(in crate::shell::settings) async fn exercise(
    settings: WindowHandle<Settings>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let original = settings.read_with(cx, |view, _| view.config.clone())?;
    settings.update(cx, |view, _, cx| {
        view.presets
            .name
            .update(cx, |field, cx| field.set("Coding".into(), cx));
        view.vocabulary.enter("cube control", "kubectl", cx);
        view.recognition.enter("Kubernetes", cx);
        ensure!(
            view.capture_preset(None, cx),
            "The named preset was not captured"
        );
        ensure!(
            !view.vocabulary.pending(cx)
                && !view.recognition.pending(cx)
                && !view.presets.pending(cx),
            "Preset capture left pending input behind"
        );
        view.config.vocabulary.clear();
        view.config.recognition_terms = Terms::default();
        anyhow::Ok(())
    })??;
    Timer::after(std::time::Duration::from_millis(100)).await;
    settings.update(cx, |view, window, cx| {
        let unmodified = view.config.clone();
        view.vocabulary.enter("unfinished", "", cx);
        view.edit_preset(Edit::Load("Coding".into()), window, cx);
        ensure!(
            view.config == unmodified && view.vocabulary.pending(cx),
            "Loading discarded an unfinished correction"
        );
        view.vocabulary.clear(cx);
        view.recognition.enter("Unfinished hint", cx);
        view.edit_preset(Edit::Load("Coding".into()), window, cx);
        ensure!(
            view.config == unmodified && view.recognition.pending(cx),
            "Loading discarded an unfinished hint"
        );
        view.recognition.clear(cx);
        view.presets
            .name
            .update(cx, |field, cx| field.set("Other preset".into(), cx));
        view.edit_preset(Edit::Load("Coding".into()), window, cx);
        ensure!(
            view.config == unmodified && view.presets.pending(cx),
            "Loading discarded an unfinished name"
        );
        view.presets.clear(cx);
        view.file_picker = Some(Task::ready(()));
        view.edit_preset(Edit::Load("Coding".into()), window, cx);
        ensure!(
            view.config == unmodified,
            "Loading raced an outstanding file picker"
        );
        view.finish_choice(Ok(None), super::super::EngineFile::Model, cx);
        view.saving = Some(Task::ready(()));
        view.edit_preset(Edit::Load("Coding".into()), window, cx);
        ensure!(
            view.config == unmodified,
            "Loading raced an earlier settings save"
        );
        view.saving = None;
        view.edit_preset(Edit::Load("Coding".into()), window, cx);
        ensure!(
            Vocabulary::new(&view.config.vocabulary).text("Use cube control.") == "Use kubectl.",
            "Loading omitted a pending correction captured with the preset"
        );
        ensure!(
            view.config.recognition_terms.phrases() == ["Kubernetes"],
            "Loading omitted a pending recognition hint"
        );
        anyhow::Ok(())
    })??;
    Timer::after(std::time::Duration::from_millis(100)).await;
    settings.update(cx, |view, window, cx| {
        view.config.language = "fr".into();
        view.vocabulary.enter("post grass", "Postgres", cx);
        view.edit_preset(Edit::Replace("Coding".into()), window, cx);
        view.config.vocabulary.clear();
        view.config.language = "en".into();
        view.edit_preset(Edit::Load("Coding".into()), window, cx);
        ensure!(
            view.config.language == "fr"
                && Vocabulary::new(&view.config.vocabulary).text("Use post grass.")
                    == "Use Postgres.",
            "Replacing a preset omitted current choices or pending corrections"
        );
        let active = view.config.vocabulary.clone();
        let stale_actions = [
            Edit::Load("Coding".into()),
            Edit::Replace("Coding".into()),
            Edit::Remove("Coding".into()),
        ];
        view.config.language = "en".into();
        view.config
            .presets
            .push(Preset::capture("Other".into(), &view.config)?);
        view.edit_preset(Edit::Remove("Coding".into()), window, cx);
        view.config.language = "de".into();
        let after_removal = view.config.clone();
        for action in stale_actions {
            view.edit_preset(action, window, cx);
            ensure!(
                view.config == after_removal,
                "A stale preset action changed another preset"
            );
        }
        view.edit_preset(Edit::Load("Other".into()), window, cx);
        ensure!(
            view.config.language == "en",
            "The surviving preset was not usable"
        );
        view.edit_preset(Edit::Remove("Other".into()), window, cx);
        ensure!(
            view.config.presets.is_empty() && view.config.vocabulary == active,
            "Deleting a saved preset changed active dictation choices"
        );
        view.presets
            .name
            .update(cx, |field, cx| field.set("Discarded draft".into(), cx));
        view.act(super::super::Action::Revert, window, cx);
        ensure!(
            !view.presets.pending(cx),
            "Revert retained an unfinished preset name"
        );
        view.config = original;
        view.notice = None;
        cx.notify();
        anyhow::Ok(())
    })??;
    Ok(())
}
