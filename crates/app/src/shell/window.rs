use std::path::{Path, PathBuf};

use anyhow::anyhow;
#[cfg(not(target_os = "windows"))]
use gpui::PathPromptOptions;
use gpui::{
    App, AppContext, Bounds, Context, Global, Pixels, Size, Subscription, Task, TitlebarOptions,
    Window, WindowBounds, WindowHandle, WindowOptions, px, size,
};
use speakeasy_platform::FileFilter;

use super::{LaunchMode, Services, settings::Settings};
#[cfg(target_os = "windows")]
use crate::gpui_ext::native_window;
use crate::{
    gpui_ext::{Presence, WindowUpdate, present},
    show::review::{self, Source},
};

#[cfg(target_os = "windows")]
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
            preview: |request, cx| {
                let services = cx.global_mut::<Services>();
                anyhow::ensure!(!services.is_quitting(), "Speakeasy is quitting.");
                services.show_previews.submit(request)
            },
            can_copy: |cx| {
                cx.try_global::<Services>()
                    .is_some_and(Services::can_copy_show)
            },
            copy: |request, cx| cx.global_mut::<Services>().copy_show(request),
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

pub(crate) fn report_focus<V: 'static>(window: &mut Window, cx: &Context<'_, V>) {
    let id = window.window_handle().window_id().as_u64();
    cx.observe_window_activation(window, move |_, window, cx| {
        settle_focus(id, window.is_window_active(), cx);
    })
    .detach();
    cx.on_release(move |_, cx| settle_focus(id, false, cx))
        .detach();
}

fn settle_focus(window: u64, focused: bool, cx: &App) {
    if let Some(services) = cx.try_global::<Services>() {
        services.focus.settle(window, focused);
    }
}

struct PillPresence {
    surface: PillSurface,
    reopening: Option<Task<()>>,
    _closed: Subscription,
    _displays: Subscription,
}

impl Global for PillPresence {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PillSurface {
    Shown,
    Reopened,
    Lost,
}

impl PillSurface {
    fn closed(self) -> Self {
        match self {
            Self::Shown => Self::Reopened,
            Self::Reopened | Self::Lost => Self::Lost,
        }
    }

    fn displays_changed(self, any_display: bool) -> Self {
        match self {
            Self::Lost if any_display => Self::Reopened,
            Self::Lost => Self::Lost,
            Self::Shown | Self::Reopened => Self::Shown,
        }
    }
}

pub(crate) fn keep_pill_presented(cx: &mut App) {
    let closed = cx.on_window_closed(|cx, window| {
        if cx
            .try_global::<Services>()
            .is_some_and(|services| services.pill.window_id() == window)
        {
            settle_pill(PillSurface::closed, cx);
        }
    });
    let displays = cx.observe_displays(|_, cx| {
        let any_display = !cx.displays().is_empty();
        settle_pill(|surface| surface.displays_changed(any_display), cx);
    });
    cx.set_global(PillPresence {
        surface: PillSurface::Shown,
        reopening: None,
        _closed: closed,
        _displays: displays,
    });
}

fn settle_pill(step: impl FnOnce(PillSurface) -> PillSurface, cx: &mut App) {
    let surface = step(cx.global::<PillPresence>().surface);
    let reopening = (surface == PillSurface::Reopened).then(|| {
        cx.spawn(async |cx| {
            cx.update(|cx| {
                if let Err(error) = reopen_pill(cx) {
                    cx.global_mut::<PillPresence>().surface = PillSurface::Lost;
                    cx.global::<Services>()
                        .report(&anyhow!("Cannot reopen the pill: {error:#}"));
                }
            });
        })
    });
    let presence = cx.global_mut::<PillPresence>();
    presence.surface = surface;
    if reopening.is_some() {
        presence.reopening = reopening;
    }
}

fn reopen_pill(cx: &mut App) -> anyhow::Result<()> {
    let services = cx.global::<Services>();
    let pill = crate::pill::open(
        services.output.subscribe(),
        services.meter.subscribe(),
        services.config.prefers_reduced_motion(),
        services.config.theme,
        crate::pill::Opening::Reopened,
        cx,
    )?;
    cx.global_mut::<Services>().pill = pill;
    Ok(())
}

pub(crate) fn quit_after_settings(cx: &mut App) {
    if settings_if_open(cx).is_none() {
        super::request_quit(cx);
    }
}

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
            let mut view = Settings::new(config, updates, window, cx);
            Settings::watch_practice_focus(window, cx);
            if !mode.is_demo() {
                view.follow_permissions(window, cx, speakeasy_platform::Permissions::read);
            }
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

const SETTINGS_SIZE: Size<Pixels> = size(px(560.0), px(860.0));

const DISPLAY_CLEARANCE: Pixels = px(180.0);

const MIN_HEIGHT: Pixels = px(360.0);

fn window_options(cx: &App) -> WindowOptions {
    let display_height = cx
        .primary_display()
        .map(|display| display.bounds().size.height)
        .or_else(|| {
            cx.displays()
                .iter()
                .map(|display| display.bounds().size.height)
                .min()
        });
    let height = display_height.map_or(SETTINGS_SIZE.height, |display| {
        (display - DISPLAY_CLEARANCE).clamp(MIN_HEIGHT, SETTINGS_SIZE.height)
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
    let presence = if visible {
        Presence::Raised
    } else {
        Presence::Hidden
    };
    let task = cx.spawn(async move |cx| {
        if present(settings.into(), presence, cx) && !visible {
            cx.update(show_tray_hint);
        }
    });
    cx.global_mut::<Services>().visibility = Some(task);
}

pub(super) fn hide_on_close(window: &mut Window, cx: &mut App) -> bool {
    if !cx.has_global::<Services>() {
        return true;
    }

    if let Some(Some(view)) = window.root::<Settings>() {
        view.update(cx, Settings::end_practice);
    }

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

fn tray_hint_marker(settings: &Path, mode: LaunchMode) -> Option<PathBuf> {
    (!mode.is_demo()).then(|| settings.with_file_name("tray-hint-seen"))
}

#[cfg(target_os = "windows")]
fn minimize_to_tray(window: &Window, cx: &App) -> anyhow::Result<Task<()>> {
    let (hide, hidden) = async_channel::bounded(1);
    native_window(window, WINDOW_NAME)?.minimize_to_tray(hide)?;
    Ok(cx.spawn(async move |cx| {
        while hidden.recv().await.is_ok() {
            cx.update(hide_to_tray);
        }
    }))
}

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

    #[test]
    fn a_closed_pill_reopens_once_until_the_displays_change() {
        let reopened = PillSurface::Shown.closed();
        assert_eq!(reopened, PillSurface::Reopened, "The pill stayed closed");
        let lost = reopened.closed();
        assert_eq!(
            lost,
            PillSurface::Lost,
            "A compositor that keeps closing the pill made it reopen in a loop"
        );
        assert_eq!(
            lost.displays_changed(false),
            PillSurface::Lost,
            "The pill reopened with no display to show it"
        );
        assert_eq!(
            lost.displays_changed(true),
            PillSurface::Reopened,
            "The pill stayed closed after a display arrived"
        );
        assert_eq!(
            reopened.displays_changed(true).closed(),
            PillSurface::Reopened,
            "A display change did not let the pill reopen again"
        );
    }
}
