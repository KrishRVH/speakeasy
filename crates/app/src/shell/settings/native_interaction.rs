use std::time::Duration;

use anyhow::{Context as _, ensure};
use gpui::{
    AppContext as _, AsyncApp, KeyDownEvent, KeyUpEvent, Keystroke, PlatformInput, WindowHandle,
    WindowOptions,
};

use super::{menu::Menu, *};
use crate::{runtime::Meter, shell::LaunchMode, show::Canvas};

const SETTLE: Duration = Duration::from_millis(100);

mod microphones;

#[test]
#[ignore = "Opt-in owned windows; Linux requires a private X11 display. No microphone, input, or clipboard"]
fn native_settings_picker_restores_focus_and_close_clears_practice() -> anyhow::Result<()> {
    let (completed, outcome) = std::sync::mpsc::channel();
    let (application, presentation) = crate::application::new().unwrap();
    application.run(move |cx| {
        cx.set_global(presentation);
        cx.spawn(async move |cx| {
            let result = async {
                let directory = tempfile::tempdir()?;
                let config = Config {
                    engine: Engine::Whisper,
                    engine_executable: "whisper-server".into(),
                    model: "ggml-base.bin".into(),
                    ..Config::default()
                };
                let (output, updates) = watch::channel(Snapshot::default());
                let (meter, levels) = watch::channel(Meter::default());
                let (canvas, _) = watch::channel(Canvas::default());
                let pill = cx.update(|cx| {
                    crate::pill::open(
                        updates.clone(),
                        levels,
                        true,
                        config.theme,
                        crate::pill::Opening::Startup,
                        cx,
                    )
                })?;
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
                });
                let settings = cx.open_window(WindowOptions::default(), |window, cx| {
                    cx.new(|cx| Settings::new(config, updates, window, cx))
                })?;
                cx.update(|cx| cx.global_mut::<Services>().settings = Some(settings));
                onboarding::native_tests::exercise(settings, cx).await?;
                presets::native_tests::exercise(settings, cx).await?;
                settings.update(cx, |view, _, cx| {
                    let applied = cx.global::<Services>().config.language.clone();
                    view.config.language = "de".into();
                    view.own_engine = true;
                    view.finish_save(
                        None,
                        Some("Synthetic save failure".into()),
                        ConfigEpoch::default(),
                        cx,
                    );
                    ensure!(
                        view.config.language == applied
                            && !view.own_engine
                            && view.notice.as_ref().is_some_and(Notice::is_problem),
                        "A failed save kept settings that were never applied"
                    );
                    view.notice = None;
                    anyhow::Ok(())
                })??;
                parakeet_cleanup_language(settings, cx)?;
                microphones::exercise(settings, cx).await?;
                settings.update(cx, |view, window, cx| {
                    view.menus.trigger(Menu::Language).focus(window, cx);
                })?;
                key(settings, "enter", cx)?;
                ensure!(
                    settings.read_with(cx, |view, _| view.menus.open == Some(Menu::Language))?,
                    "The toggle's release immediately selected a language"
                );
                settle(cx).await;
                key(settings, "end", cx)?;
                settle(cx).await;
                settings.update(cx, |view, _, _| {
                    ensure!(
                        view.menus.scroll.offset().y < gpui::px(0.0),
                        "The last language remained off screen"
                    );
                    anyhow::Ok(())
                })??;
                key(settings, "enter", cx)?;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.config.language == "yo",
                        "Keyboard selection missed the last language"
                    );
                    ensure!(view.menus.open.is_none(), "Key release reopened the picker");
                    ensure!(
                        view.menus.trigger(Menu::Language).is_focused(window),
                        "Selection lost focus"
                    );
                    view.toggle_menu(Menu::Language, window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                key(settings, "space", cx)?;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.menus.open.is_none(),
                        "Space release reopened the picker"
                    );
                    view.toggle_menu(Menu::Language, window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                key(settings, "escape", cx)?;
                settings.update(cx, |view, window, cx| {
                    ensure!(view.menus.open.is_none(), "Escape left the picker open");
                    ensure!(
                        view.menus.trigger(Menu::Language).is_focused(window),
                        "Escape lost picker focus"
                    );
                    activate(window)?;
                    view.toggle_menu(Menu::Language, window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                settle(cx).await;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        window.is_window_active(),
                        "The owned focus fixture did not activate"
                    );
                    ensure!(
                        cx.global::<Services>().focus.subscribe().held(),
                        "Active Settings left insertion free to type into it"
                    );
                    view.menus.trigger(Menu::Language).focus(window, cx);
                    anyhow::Ok(())
                })??;
                settle(cx).await;
                key(settings, "space", cx)?;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.menus.open.is_none(),
                        "Returning to the toggle reopened the picker"
                    );
                    view.toggle_menu(Menu::Language, window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                settle(cx).await;
                settings.update(cx, |view, window, cx| view.menus.group.focus(window, cx))?;
                key(settings, "home", cx)?;
                key(settings, "enter", cx)?;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.config.language == "auto" && view.menus.open.is_none(),
                        "The picker group ignored keyboard navigation"
                    );
                    view.toggle_menu(Menu::Language, window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                settle(cx).await;
                settings.update(cx, |_, window, cx| window.blur(cx))?;
                settle(cx).await;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        view.menus.open.is_none(),
                        "Losing focus left the picker open"
                    );
                    view.toggle_menu(Menu::Language, window, cx);
                    cx.notify();
                    anyhow::Ok(())
                })??;
                settle(cx).await;
                let other = cx.open_window(WindowOptions::default(), |_, cx| {
                    cx.new(|_| gpui::EmptyView)
                })?;
                settle(cx).await;
                other.update(cx, |_, window, _| activate(window))??;
                settle(cx).await;
                settings.update(cx, |view, window, cx| {
                    ensure!(
                        !window.is_window_active(),
                        "The second owned window did not activate"
                    );
                    ensure!(
                        !cx.global::<Services>().focus.subscribe().held(),
                        "Inactive Settings still blocked insertion"
                    );
                    ensure!(
                        view.menus.open.is_none(),
                        "Deactivation left the picker open"
                    );
                    view.follow_permissions(window, cx, grant_microphone_on_return());
                    ensure!(
                        view.permissions
                            .map(|snapshot| permissions::description(snapshot.microphone))
                            == Some("Blocked"),
                        "A denied microphone permission was presented as allowed"
                    );
                    activate(window)
                })??;
                settle(cx).await;
                settings.update(cx, |view, window, _| {
                    ensure!(
                        view.menus.trigger(Menu::Language).is_focused(window),
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
                        "Refreshing permission lost a language edit"
                    );
                    view.practice.text =
                        zeroize::Zeroizing::new("Synthetic private practice".into());
                    view.practice.ready = true;
                    view.practice.task = Some(Task::ready(()));
                    anyhow::Ok(())
                })??;
                navigate(settings, cx).await?;
                other.update(cx, |_, window, _| window.remove_window())?;

                cx.update(|cx| close(settings, cx))?;
                settings.update(cx, |view, _, _| {
                    ensure!(
                        view.practice.text.is_empty(),
                        "Closing retained private practice text"
                    );
                    ensure!(!view.practice.ready, "Closing retained practice readiness");
                    ensure!(
                        view.practice.task.is_none(),
                        "Closing retained the practice task"
                    );
                    anyhow::Ok(())
                })??;
                anyhow::Ok(())
            }
            .await;
            completed.send(result).unwrap();
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    outcome
        .try_recv()
        .context("Native interaction acceptance did not complete")?
}

fn parakeet_cleanup_language(
    settings: WindowHandle<Settings>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let original = settings.read_with(cx, |view, _| view.config.clone())?;
    settings.update(cx, |view, window, cx| {
        view.config.engine = Engine::Nemo;
        view.config.language = "de-DE".into();
        ensure!(
            speech::detects_language(&view.config),
            "Parakeet offered a language choice instead of detecting it"
        );
        ensure!(
            !Setting::RemoveFillers.is_on(&view.config),
            "A German locale presented um and uh removal as on"
        );
        view.act(Action::Toggle(Setting::RemoveFillers), window, cx);
        ensure!(
            view.config.language == "en"
                && view.config.fillers(&view.config.language) == crate::transcript::Fillers::Remove,
            "Turning on um and uh removal left Parakeet without English cleanup"
        );
        view.act(Action::Toggle(Setting::RemoveFillers), window, cx);
        ensure!(
            view.config.language == "auto"
                && view.config.fillers(&view.config.language)
                    == crate::transcript::Fillers::Preserve,
            "Turning off um and uh removal kept removing words"
        );
        view.config = original;
        cx.notify();
        anyhow::Ok(())
    })??;
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "The owned-window fixture dispatches keys and checks focus on GPUI's UI thread"
)]
async fn navigate(settings: WindowHandle<Settings>, cx: &mut AsyncApp) -> anyhow::Result<()> {
    use super::navigation::Page;

    for (page, activation) in [
        (Page::General, "enter"),
        (Page::Words, "space"),
        (Page::Dictation, "enter"),
    ] {
        settings.update(cx, |view, window, cx| {
            view.navigation.focus(page).focus(window, cx);
        })?;
        settle(cx).await;
        key(settings, activation, cx)?;
        settle(cx).await;
        settings.update(cx, |view, window, _| {
            ensure!(
                view.navigation.page == page,
                "Keyboard navigation missed its page"
            );
            ensure!(
                view.navigation.focus(page).is_focused(window),
                "Page change lost keyboard focus"
            );
            ensure!(
                view.practice.task.is_none()
                    && view.practice.text.is_empty()
                    && !view.practice.ready,
                "Leaving the dictation page retained private practice"
            );
            ensure!(
                view.config.language == "auto",
                "Changing pages lost a language edit"
            );
            anyhow::Ok(())
        })??;
    }
    settings.update(cx, |view, _, _| {
        view.practice.text = zeroize::Zeroizing::new("Synthetic private practice".into());
        view.practice.ready = true;
        view.practice.task = Some(Task::ready(()));
    })?;
    Ok(())
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

pub(super) fn settle(cx: &AsyncApp) -> Task<()> {
    cx.background_executor().timer(SETTLE)
}

fn close(settings: WindowHandle<Settings>, cx: &mut App) -> anyhow::Result<()> {
    cx.update_window(settings.into(), |_, window, cx| {
        crate::shell::window::hide_on_close(window, cx);
    })
}

fn key(
    settings: WindowHandle<Settings>,
    name: &'static str,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let keystroke = Keystroke::parse(name)?;
    cx.update_window(settings.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            }),
            cx,
        );
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
    })
}

#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "Linux focuses the owned window through a fallible X11 connection; other platforms share its signature"
    )
)]
fn activate(window: &gpui::Window) -> anyhow::Result<()> {
    cfg_select! {
        target_os = "linux" => {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            use x11rb::{
                CURRENT_TIME,
                connection::Connection as _,
                protocol::xproto::{ConnectionExt as _, InputFocus},
                xcb_ffi::XCBConnection,
            };
            let RawWindowHandle::Xcb(owned) = HasWindowHandle::window_handle(window)?.as_raw()
            else {
                anyhow::bail!("The owned fixture is not an XCB window");
            };
            let (connection, _) = XCBConnection::connect(None)?;
            connection
                .set_input_focus(InputFocus::POINTER_ROOT, owned.window.get(), CURRENT_TIME)?
                .check()?;
            connection.flush()?;
        },
        _ => window.activate_window(),
    }
    Ok(())
}
