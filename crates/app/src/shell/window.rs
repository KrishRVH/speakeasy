//! Opening, showing, and hiding the Settings window, and its file picker. Native handles are
//! resolved only when used, in tasks that run after GPUI releases the window borrow.

use std::path::PathBuf;

use gpui::{
    App, AppContext, Bounds, PathPromptOptions, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, px, size,
};

use super::{Services, settings::Settings};
use crate::gpui_ext::{AppUpdate, WindowUpdate, raw_handle};

/// Names the Settings window in errors about its native handle.
const WINDOW_NAME: &str = "Settings";

pub(crate) fn reveal(cx: &mut App) {
    if !cx.has_global::<Services>() {
        return;
    }
    if let Err(error) = open(cx) {
        speakeasy_platform::show_error(&format!("{error:#}"));
    }
}

fn open(cx: &mut App) -> anyhow::Result<()> {
    // Reading succeeds only while the window is open.
    if let Some(settings) = cx.global::<Services>().settings
        && settings.read(cx).is_ok()
    {
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
            if fresh_install {
                view.start_setup(cx);
            }
            view
        })
    })?;
    cx.global_mut::<Services>().settings = Some(settings);
    if mode.is_demo() {
        settings.update_if_open(cx, |view, _, cx| view.play(cx));
    }
    Ok(())
}

fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(560.0), px(720.0)),
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
    let task = cx.spawn(async move |cx| {
        let resolved = settings.update(cx, |_, window, _| raw_handle(window, WINDOW_NAME));
        if let Ok(Ok(raw)) = resolved {
            speakeasy_platform::set_settings_visible(raw, visible);
            if visible {
                cx.update_if_running(|cx| cx.activate(true));
            } else {
                cx.update_if_running(show_tray_hint);
            }
        }
    });
    // Replacing the task cancels a queued hide when Settings is reopened first.
    cx.global_mut::<Services>().visibility = Some(task);
}

fn hide_on_close(_: &mut Window, cx: &mut App) -> bool {
    if !cx.has_global::<Services>() {
        return true;
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

fn show_tray_hint(cx: &mut App) {
    let services = cx.global::<Services>();
    let marker = services.path.with_file_name("tray-hint-seen");
    if services.tray_hint_seen || marker.exists() {
        return;
    }
    let pill = services.pill;
    if pill
        .update(cx, |pill, _, cx| pill.tray_hint(cx))
        .unwrap_or(false)
    {
        cx.global_mut::<Services>().tray_hint_seen = true;
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The hint remains dismissed in this app; persistence failure may only show it again after relaunch"
        )]
        let _ = std::fs::write(marker, b"");
    }
}

/// Asks for one file. GPUI's picker takes no file-type filter, so `filter` is unused.
pub(super) fn choose_file(
    _: &Window,
    title: &'static str,
    _: [&'static str; 2],
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
