use std::time::Duration;

use anyhow::{Context as _, ensure};
use gpui::{
    AppContext as _, Application, Bounds, EntityInputHandler as _, WindowBounds, WindowOptions,
    point, px, size,
};

use super::*;
use crate::{config::valid_config, gpui_ext::AppUpdate as _, theme::Theme};

#[test]
#[ignore = "Opt-in owned windows; Linux requires a private X11 display. No microphone, input, or clipboard"]
fn native_phrase_editor_handles_ime_and_settings_drop_finishes_queued_saves() -> anyhow::Result<()>
{
    let (completed, outcome) = std::sync::mpsc::channel();
    Application::new().run(move |cx| {
        cx.spawn(async move |cx| {
            let result = async {
                let directory = tempfile::tempdir()?;
                let path = directory.path().join("settings.json");
                let config = valid_config(directory.path())?;
                let mut saves = SaveQueue::new(path.clone(), config.clone());
                ensure!(
                    saves
                        .submit(config.clone(), ConfigEpoch::default())
                        .is_some(),
                    "The first settings save did not start"
                );
                ensure!(
                    saves
                        .submit(
                            Config {
                                threads: 8,
                                ..config.clone()
                            },
                            ConfigEpoch::default()
                        )
                        .is_none(),
                    "The pending settings save started before the first finished"
                );
                let input = cx.new(|cx| {
                    text_field::TextField::new("Synthetic field", Theme::default(), cx)
                })?;
                let window = cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.0), px(0.0)),
                            size(px(300.0), px(60.0)),
                        ))),
                        ..Default::default()
                    },
                    |_, _| input,
                )?;
                let view = window.update(cx, |_, window, cx| {
                    cx.new(|cx| Settings {
                        vocabulary: vocabulary::Editor::new(config.theme, window, cx),
                        recognition: recognition::Editor::new(config.theme, window, cx),
                        presets: presets::Editor::new(config.theme, window, cx),
                        menus: menu::Menus::new(cx),
                        navigation: navigation::Navigation::new(cx),
                        practice: practice::Practice::default(),
                        onboarding: None,
                        onboarding_focus: cx.focus_handle(),
                        onboarding_completion: None,
                        permissions: None,
                        update: Update::Idle,
                        config,
                        presentation: Presentation {
                            indicator: Indicator::Paused,
                            shortcut: Arc::from(""),
                            cancel_shortcut: Arc::from(""),
                            message: String::new(),
                            model: None,
                        },
                        notice: None,
                        microphones: Vec::new(),
                        file_picker: None,
                        microphone_listing: None,
                        preview: None,
                        setup: None,
                        retiring_setup: None,
                        saving: None,
                        saves,
                        saved: None,
                        _updates: Task::ready(()),
                        unreadable: false,
                        reset: super::Reset::Idle,
                        downloaded: Vec::new(),
                        own_engine: false,
                        choosing_model: false,
                        shows_help: false,
                        keyboard: false,
                        startup_apps_off: false,
                        #[cfg(target_os = "linux")]
                        desktop_command: SharedString::default(),
                        #[cfg(target_os = "windows")]
                        minimize_to_tray: None,
                    })
                })?;
                drop(view);
                cx.update(|_| {})?;
                ensure!(
                    Config::read(&path)?.threads == 8,
                    "Settings drop lost the queued save"
                );
                window.update(cx, |field, window, cx| {
                    field.set("🦀 café ".into(), cx);
                    field.replace_and_mark_text_in_range(None, "日本", Some(1..2), window, cx);
                    let selection = field
                        .selected_text_range(false, window, cx)
                        .context("The marked composition has no selection")?;
                    ensure!(
                        selection.range == (9..10),
                        "The marked composition has the wrong selection"
                    );
                    composition_keys(field, window, cx)?;
                    field.replace_text_in_range(None, "日本語", window, cx);
                    ensure!(
                        field.text() == "🦀 café 日本語",
                        "Committing the composition changed its text"
                    );
                    field.select_accessible(
                        gpui::AccessibleTextSelection {
                            anchor: field.text().len(),
                            focus: 4,
                        },
                        window,
                        cx,
                    );
                    let selected = field
                        .selected_text_range(false, window, cx)
                        .context("The accessible selection was lost")?;
                    ensure!(
                        selected.reversed,
                        "The accessible selection lost its direction"
                    );
                    ensure!(
                        selected.range == (2..11),
                        "The accessible selection has the wrong range"
                    );
                    anyhow::Ok(())
                })??;
                gpui::Timer::after(Duration::from_millis(100)).await;
                window.update(cx, |field, window, cx| {
                    let bounds = field.bounds_for_range(8..11, Bounds::default(), window, cx);
                    ensure!(bounds.is_some(), "The field did not shape its text");
                    window.remove_window();
                    anyhow::Ok(())
                })??;
                anyhow::Ok(())
            }
            .await;
            completed.send(result).unwrap();
            cx.update_if_running(|cx| cx.quit());
        })
        .detach();
    });
    outcome
        .try_recv()
        .context("Phrase editor acceptance did not complete")?
}

fn composition_keys(
    field: &mut text_field::TextField,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<'_, text_field::TextField>,
) -> anyhow::Result<()> {
    field.select_accessible(
        gpui::AccessibleTextSelection {
            anchor: 0,
            focus: 0,
        },
        window,
        cx,
    );
    for key in ["backspace", "delete", "left", "right", "enter"] {
        field.key_down(
            &gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse(key)?,
                is_held: false,
            },
            window,
            cx,
        );
        ensure!(
            field.text() == "🦀 café 日本",
            "A key changed the active composition text"
        );
        ensure!(
            field.marked_text_range(window, cx) == Some(8..10),
            "A key changed the marked composition range"
        );
        let selected = field
            .selected_text_range(false, window, cx)
            .context("A key removed the composition selection")?;
        ensure!(
            selected.range == (9..10),
            "A key changed the composition selection"
        );
    }
    Ok(())
}
