//! Opening, showing, and hiding the Settings window, and its file picker.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
#[cfg(not(target_os = "windows"))]
use gpui::PathPromptOptions;
use gpui::{
    App, AppContext, Bounds, Pixels, Size, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, px, size,
};
use speakeasy_platform::FileFilter;

use super::{LaunchMode, Services, settings::Settings};
#[cfg(target_os = "windows")]
use crate::gpui_ext::native_window;
use crate::{
    gpui_ext::{AppUpdate, WindowUpdate, defer_native},
    show::review::{self, Source},
};

/// Names the Settings window in errors about its native handle.
const WINDOW_NAME: &str = "Settings";

pub(crate) fn reveal(cx: &mut App) {
    if !cx.has_global::<Services>() {
        return;
    }
    if let Err(error) = open(cx) {
        cx.global::<Services>()
            .report(&anyhow!("Cannot open Settings: {error:#}"));
    }
}

pub(crate) fn reveal_shows(cx: &mut App) {
    let Some(services) = cx.try_global::<Services>() else {
        return;
    };
    if services.is_quitting() {
        return;
    }
    if let Some(shows) = services.shows.filter(|shows| shows.read(cx).is_ok()) {
        shows.update_if_open(cx, |view, window, cx| view.reveal(window, cx));
        return;
    }
    let source = if services.mode.is_demo() {
        Source::Demo
    } else {
        Source::Saved
    };
    let result = review::open(
        source,
        services.config.theme,
        services.output.subscribe(),
        review::Actions {
            edit: |edit, fixture, cx| {
                let services = cx.global_mut::<Services>();
                anyhow::ensure!(!services.is_quitting(), "Speakeasy is quitting.");
                services.show_edits.submit(edit, fixture)
            },
            can_copy: |cx| {
                cx.try_global::<Services>()
                    .is_some_and(Services::can_copy_show)
            },
            recover: |show, cx| {
                cx.try_global::<Services>()
                    .ok_or("Resume Speakeasy to recover this Show.")?
                    .recover_show(show)
            },
        },
        cx,
    );
    match result {
        Ok(shows) => cx.global_mut::<Services>().shows = Some(shows),
        Err(error) => cx
            .global::<Services>()
            .report(&anyhow!("Cannot open Shows: {error:#}")),
    }
}

/// Ends a launch without a tray once its Settings window has closed. Other windows come and go
/// without ending it, such as the Show overlay, which closes after every Show.
pub(crate) fn quit_after_settings(cx: &mut App) {
    if settings_if_open(cx).is_none() {
        super::request_quit(cx);
    }
}

/// The Settings window while it is open, shown or hidden. A window is gone before observers of its
/// closing run, so reading it fails once it has closed.
fn settings_if_open(cx: &App) -> Option<WindowHandle<Settings>> {
    cx.try_global::<Services>()?
        .settings
        .filter(|settings| settings.read(cx).is_ok())
}

pub(super) fn open(cx: &mut App) -> anyhow::Result<()> {
    if let Some(settings) = settings_if_open(cx) {
        set_visible(settings, true, cx);
        return Ok(());
    }
    let services = cx.global::<Services>();
    let config = services.config.clone();
    let mode = services.mode;
    let fresh_install = !mode.is_demo() && !services.path.exists();
    let updates = services.output.subscribe();
    let settings = cx.open_window(window_options(cx), |window, cx| {
        if mode.is_resident() {
            window.on_window_should_close(cx, hide_on_close);
        }
        cx.new(|cx| {
            let mut view = Settings::new(config, updates, cx);
            Settings::watch_practice_focus(window, cx);
            if fresh_install {
                view.start_setup(cx);
            }
            view
        })
    })?;
    #[cfg(target_os = "windows")]
    if mode.is_resident() {
        settings.update(cx, |view, window, cx| {
            let hook = minimize_to_tray(window, cx).inspect_err(|_| window.remove_window())?;
            view.minimize_to_tray = Some(hook);
            anyhow::Ok(())
        })??;
    }
    #[cfg(target_os = "windows")]
    if mode == LaunchMode::Live && cx.global::<Services>().default_settings {
        settings.update(cx, |view, window, cx| view.follow_startup_apps(window, cx))?;
    }
    cx.global_mut::<Services>().settings = Some(settings);
    if mode.is_demo() {
        settings.update_if_open(cx, |view, _, cx| view.play(cx));
    }
    Ok(())
}

/// Settings' size where the display has room: tall enough to show every control without scrolling.
const SETTINGS_SIZE: Size<Pixels> = size(px(560.0), px(860.0));
/// What a smaller display keeps free around Settings, centered, for its title bar, a menu bar, and
/// a taskbar or dock.
const DISPLAY_CLEARANCE: Pixels = px(180.0);
/// Settings never opens shorter than this; its controls scroll above the pinned commands.
const MIN_HEIGHT: Pixels = px(360.0);

fn window_options(cx: &App) -> WindowOptions {
    let height = cx
        .primary_display()
        .map_or(SETTINGS_SIZE.height, |display| {
            (display.bounds().size.height - DISPLAY_CLEARANCE)
                .clamp(MIN_HEIGHT, SETTINGS_SIZE.height)
        });
    WindowOptions {
        #[cfg(target_os = "linux")]
        app_id: Some(speakeasy_platform::APPLICATION_ID.into()),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(SETTINGS_SIZE.width, height),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Speakeasy".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn set_visible(settings: WindowHandle<Settings>, visible: bool, cx: &mut App) {
    if !visible {
        settings.update_if_open(cx, |view, _, cx| view.end_practice(cx));
    }
    let task = defer_native(settings.into(), WINDOW_NAME, cx, move |native, cx| {
        native.set_window_visible(visible);
        if visible {
            cx.update_if_running(|cx| cx.activate(true));
        } else {
            cx.update_if_running(show_tray_hint);
        }
    });
    // Replacing the task cancels a queued hide when Settings is reopened first.
    cx.global_mut::<Services>().visibility = Some(task);
}

pub(super) fn hide_on_close(window: &mut Window, cx: &mut App) -> bool {
    if !cx.has_global::<Services>() {
        return true;
    }
    // The close callback already borrows this window, so the handle cannot update it recursively.
    if let Some(Some(view)) = window.root::<Settings>() {
        view.update(cx, Settings::end_practice);
    }
    // Without a working tray icon, nothing could bring a hidden Settings back.
    #[cfg(target_os = "linux")]
    if !cx
        .try_global::<crate::tray::Tray>()
        .is_some_and(|tray| tray.available)
    {
        super::request_quit(cx);
        return false;
    }
    hide_to_tray(cx);
    false
}

fn hide_to_tray(cx: &mut App) {
    if let Some(settings) = cx
        .try_global::<Services>()
        .and_then(|services| services.settings)
    {
        set_visible(settings, false, cx);
    }
}

/// Shows the first-hide hint once.
fn show_tray_hint(cx: &mut App) {
    let services = cx.global::<Services>();
    let marker = tray_hint_marker(&services.path, services.mode);
    if services.tray_hint_seen || marker.as_deref().is_some_and(Path::exists) {
        return;
    }
    let pill = services.pill;
    if pill
        .update(cx, |pill, _, cx| pill.show_tray_hint(cx))
        .unwrap_or(false)
    {
        cx.global_mut::<Services>().tray_hint_seen = true;
        if let Some(marker) = marker {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The hint remains dismissed in this app; persistence failure may only show it again after relaunch"
            )]
            let _ = std::fs::write(marker, b"");
        }
    }
}

/// The file beside `settings` that remembers the tray hint across launches, so the user's settings
/// file is never rewritten for it. A demo records no hint there, though a tray preview takes the
/// instance lock beside the settings, so it shows the hint once per launch.
fn tray_hint_marker(settings: &Path, mode: LaunchMode) -> Option<PathBuf> {
    (!mode.is_demo()).then(|| settings.with_file_name("tray-hint-seen"))
}

#[cfg(target_os = "windows")]
fn minimize_to_tray(window: &Window, cx: &App) -> anyhow::Result<gpui::Task<()>> {
    let (hide, hidden) = async_channel::bounded(1);
    native_window(window, WINDOW_NAME)?.minimize_to_tray(hide)?;
    Ok(cx.spawn(async move |cx| {
        while hidden.recv().await.is_ok() {
            if cx.update(hide_to_tray).is_err() {
                break;
            }
        }
    }))
}

/// Asks for one file of the type `filter` describes.
///
/// Windows gets the platform dialog: GPUI shows its picker inside the UI thread's message loop,
/// where it stays unpainted while GPUI is idle.
#[cfg(target_os = "windows")]
pub(super) fn choose_file(
    window: &Window,
    title: &'static str,
    filter: FileFilter<'static>,
    _: &App,
) -> impl Future<Output = anyhow::Result<Option<PathBuf>>> + use<> {
    let picker = native_window(window, WINDOW_NAME).map(|owner| owner.choose_file(title, filter));
    async move { picker?.await }
}

/// GPUI's picker takes no file-type filter.
#[cfg(not(target_os = "windows"))]
pub(super) fn choose_file(
    _: &Window,
    title: &'static str,
    _: FileFilter<'static>,
    cx: &App,
) -> impl Future<Output = anyhow::Result<Option<PathBuf>>> + use<> {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(title.into()),
    });
    async move { Ok(paths.await??.and_then(|paths| paths.into_iter().next())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_live_launch_remembers_the_tray_hint_beside_its_settings() {
        let settings = Path::new("/home/user/.config/speakeasy/settings.json");
        assert_eq!(
            tray_hint_marker(settings, LaunchMode::Live).as_deref(),
            Some(Path::new("/home/user/.config/speakeasy/tray-hint-seen"))
        );
        assert_eq!(
            tray_hint_marker(settings, LaunchMode::DemoTray),
            None,
            "A tray preview wrote beside the user's settings"
        );
    }
}
