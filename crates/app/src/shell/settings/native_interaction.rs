//! Owned-window acceptance of picker focus and the borrowed-window close callback. No devices or
//! system input are used; private practice text and permission observations are synthetic fixtures.

use std::time::Duration;

use anyhow::{Context as _, ensure};
use gpui::{
    AppContext as _, Application, AsyncApp, KeyDownEvent, KeyUpEvent, Keystroke, PlatformInput,
    WindowHandle, WindowOptions,
};

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
                settings.update(cx, |view, window, _| view.language.button.focus(window))?;
                key(settings, "enter", cx)?;
                ensure!(
                    settings.read_with(cx, |view, _| view.language.cursor.is_some())?,
                    "The toggle's release immediately selected a language"
                );
                Timer::after(Duration::from_millis(100)).await;
                key(settings, "end", cx)?;
                Timer::after(Duration::from_millis(100)).await;
                settings.update(cx, |view, _, _| {
                    assert!(
                        view.language.scroll.offset().y < gpui::px(0.0),
                        "The last language remained off screen"
                    );
                })?;
                key(settings, "enter", cx)?;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.config.language == "yo",
                        "Keyboard selection missed the last language"
                    );
                    ensure!(
                        view.language.cursor.is_none(),
                        "Key release reopened the picker"
                    );
                    ensure!(
                        view.language.button.is_focused(window),
                        "Selection lost focus"
                    );
                    view.toggle_language(window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                key(settings, "space", cx)?;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.language.cursor.is_none(),
                        "Space release reopened the picker"
                    );
                    view.toggle_language(window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                key(settings, "escape", cx)?;
                settings.update(cx, |view, window, cx| {
                    assert!(view.language.cursor.is_none());
                    assert!(view.language.button.is_focused(window));
                    window.activate_window();
                    view.toggle_language(window, cx);
                    cx.notify();
                })?;
                Timer::after(Duration::from_millis(100)).await;
                settings.update(cx, |view, window, _| {
                    ensure!(
                        window.is_window_active(),
                        "The owned focus fixture did not activate"
                    );
                    view.language.button.focus(window);
                    anyhow::Ok(())
                })??;
                Timer::after(Duration::from_millis(100)).await;
                key(settings, "space", cx)?;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.language.cursor.is_none(),
                        "Returning to the toggle reopened the picker"
                    );
                    view.toggle_language(window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                Timer::after(Duration::from_millis(100)).await;
                settings.update(cx, |view, window, _| view.language.group.focus(window))?;
                key(settings, "home", cx)?;
                key(settings, "enter", cx)?;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.config.language == "auto" && view.language.cursor.is_none(),
                        "The picker group ignored keyboard navigation"
                    );
                    view.toggle_language(window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                Timer::after(Duration::from_millis(100)).await;
                settings.update(cx, |_, window, _| window.blur())?;
                Timer::after(Duration::from_millis(100)).await;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.language.cursor.is_none(),
                        "Losing focus left the picker open"
                    );
                    view.toggle_language(window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                Timer::after(Duration::from_millis(100)).await;
                let other = cx.open_window(WindowOptions::default(), |_, cx| {
                    cx.new(|_| gpui::EmptyView)
                })?;
                Timer::after(Duration::from_millis(100)).await;
                other.update(cx, |_, window, _| window.activate_window())?;
                Timer::after(Duration::from_millis(100)).await;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        !window.is_window_active(),
                        "The second owned window did not activate"
                    );
                    ensure!(
                        view.language.cursor.is_none(),
                        "Deactivation left the picker open"
                    );
                    view.follow_permissions(window, cx, grant_microphone_on_return());
                    ensure!(
                        view.permissions
                            .map(|snapshot| permissions::description(snapshot.microphone))
                            == Some("Not enabled"),
                        "A denied microphone permission was presented as allowed"
                    );
                    window.activate_window();
                    anyhow::Ok(())
                })??;
                Timer::after(Duration::from_millis(100)).await;
                settings.update(cx, |view, window, _| {
                    ensure!(
                        view.language.button.is_focused(window),
                        "Reactivation lost picker focus"
                    );
                    ensure!(
                        view.permissions
                            .map(|snapshot| permissions::description(snapshot.microphone))
                            == Some("Allowed"),
                        "Returning from system settings did not refresh microphone permission"
                    );
                    ensure!(
                        view.config.language == "auto",
                        "Refreshing permission lost an unsaved language edit"
                    );
                    view.practice.text =
                        zeroize::Zeroizing::new("Synthetic private practice".into());
                    view.practice.ready = true;
                    view.practice.task = Some(Task::ready(()));
                    anyhow::Ok(())
                })??;
                other.update(cx, |_, window, _| window.remove_window())?;
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

fn grant_microphone_on_return() -> impl FnMut() -> Permissions {
    use speakeasy_platform::Permission;

    let mut microphone = Permission::NotGranted;
    move || {
        let snapshot = Permissions {
            microphone,
            ..Permissions::default()
        };
        microphone = Permission::Granted;
        snapshot
    }
}

fn close(settings: WindowHandle<Settings>, cx: &mut App) -> anyhow::Result<()> {
    cx.update_window(settings.into(), |_, window, cx| {
        crate::shell::window::hide_on_close(window, cx);
    })
}

fn key(settings: WindowHandle<Settings>, name: &str, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let keystroke = Keystroke::parse(name)?;
    cx.update_window(settings.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
            }),
            cx,
        );
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
    })
}
