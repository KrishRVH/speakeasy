//! Owned-window acceptance of picker focus and the borrowed-window close callback. No devices or
//! system input are used; private practice text is a synthetic fixture.

use std::time::Duration;

use anyhow::Context as _;
use gpui::{AppContext as _, Application, KeyDownEvent, Keystroke, WindowHandle, WindowOptions};

use super::*;
use crate::{gpui_ext::AppUpdate as _, runtime::Meter, shell::LaunchMode, show::Canvas};

#[test]
#[ignore = "Opt-in owned windows; Linux requires a private X11 display. No microphone, input, or clipboard"]
fn native_settings_picker_restores_focus_and_close_clears_practice() -> anyhow::Result<()> {
    let (completed, outcome) = std::sync::mpsc::channel();
    Application::new().run(move |cx| {
        cx.spawn(async move |cx| {
            let result = async {
                let directory = tempfile::tempdir()?;
                let config = Config {
                    engine: Engine::Whisper,
                    ..Config::default()
                };
                let (output, updates) = watch::channel(Snapshot::default());
                let (meter, levels) = watch::channel(Meter::default());
                let (canvas, _) = watch::channel(Canvas::default());
                let pill = cx.update(|cx| {
                    crate::pill::open(updates.clone(), levels, true, config.theme, cx)
                })??;
                cx.update(|cx| {
                    cx.set_global(Services::new(
                        directory.path().join("settings.json"),
                        false,
                        config.clone(),
                        output,
                        meter,
                        canvas,
                        pill,
                        None,
                        LaunchMode::DemoTray,
                    ));
                })?;
                let settings = cx.open_window(WindowOptions::default(), |_, cx| {
                    cx.new(|cx| Settings::new(config, updates, cx))
                })?;
                cx.update(|cx| cx.global_mut::<Services>().settings = Some(settings))?;
                settings.update(cx, |view, window, cx| {
                    view.toggle_language(window);
                    cx.notify();
                })?;
                Timer::after(Duration::from_millis(100)).await;
                settings.update(cx, |view, window, cx| {
                    assert!(view.language.menu.is_focused(window));
                    view.language_key(&key("end"), window, cx);
                })?;
                Timer::after(Duration::from_millis(100)).await;
                settings.update(cx, |view, window, cx| {
                    assert!(
                        view.language.scroll.offset().y < gpui::px(0.0),
                        "The last language remained off screen"
                    );
                    view.language_key(&key("enter"), window, cx);
                    assert_eq!(view.config.language, "yo");
                    assert!(view.language.cursor.is_none());
                    assert!(view.language.button.is_focused(window));
                    view.toggle_language(window);
                    view.language_key(&key("escape"), window, cx);
                    assert!(view.language.cursor.is_none());
                    assert!(view.language.button.is_focused(window));
                    view.practice.text =
                        zeroize::Zeroizing::new("Synthetic private practice".into());
                    view.practice.ready = true;
                    view.practice.task = Some(Task::ready(()));
                })?;
                // GPUI invokes should-close with the window already borrowed, but without borrowing
                // the Settings entity. Recursing through WindowHandle::update silently misses it.
                cx.update(|cx| close(settings, cx))??;
                settings.update(cx, |view, _, _| {
                    assert!(view.practice.text.is_empty());
                    assert!(!view.practice.ready);
                    assert!(view.practice.task.is_none());
                })?;
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
        .context("Native interaction acceptance did not complete")?
}

fn close(settings: WindowHandle<Settings>, cx: &mut App) -> anyhow::Result<()> {
    cx.update_window(settings.into(), |_, window, cx| {
        crate::shell::window::hide_on_close(window, cx);
    })
}

fn key(name: &str) -> KeyDownEvent {
    KeyDownEvent {
        keystroke: Keystroke {
            key: name.into(),
            ..Keystroke::default()
        },
        is_held: false,
    }
}
