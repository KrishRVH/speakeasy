use std::time::Duration;

use gpui::{App, AsyncApp, Timer, WindowHandle};

#[cfg(target_os = "windows")]
use super::services::Installation;
use super::{Services, settings::Settings};
use crate::gpui_ext::{AppUpdate, WindowUpdate};

const DRAIN_POLL: Duration = Duration::from_millis(10);

#[cfg(test)]
#[cfg(target_os = "windows")]
mod native_tests;

#[cfg(all(feature = "native-acceptance", target_os = "macos"))]
pub(super) mod mac_acceptance;

pub(crate) fn request_quit(cx: &mut App) {
    begin_quit(
        cx,
        #[cfg(target_os = "windows")]
        None,
    );
}

#[cfg(target_os = "windows")]
pub(crate) fn request_install(installer: crate::updates::Installer, cx: &mut App) {
    begin_quit(cx, Some(installer));
}

fn begin_quit(
    cx: &mut App,
    #[cfg(target_os = "windows")] installer: Option<crate::updates::Installer>,
) {
    if !cx.has_global::<Services>() {
        cx.quit();
        return;
    }
    let services = cx.global_mut::<Services>();
    let Some(stopped) = services.begin_quit() else {
        return;
    };
    #[cfg(target_os = "windows")]
    {
        services.installation = installer.map(Installation::Waiting);
    }
    let settings = services.settings;
    let shows = services.shows;
    let task = cx.spawn(async move |cx| {
        if let Some(shows) = shows {
            shows.update_if_open(cx, |_, window, _| window.remove_window());
        }

        if let Some(settings) = settings {
            settings.update_if_open(cx, |view, _, _| view.cancel_background_work());
        }
        stopped.await;
        while !drained(settings, cx) {
            Timer::after(DRAIN_POLL).await;
        }
        cx.update_if_running(|cx| {
            let services = cx.global_mut::<Services>();
            services.finish_quit();
            #[cfg(target_os = "windows")]
            if let Some(Installation::Waiting(installer)) = services.installation.take() {
                services.installation = Some(Installation::Ready(installer));
            }
            cx.quit();
        });
    });
    cx.global_mut::<Services>().quit = Some(task);
}

fn drained(settings: Option<WindowHandle<Settings>>, cx: &mut AsyncApp) -> bool {
    settings.is_none_or(|settings| {
        settings
            .update(cx, |view, _, _| view.drain_finished())
            .unwrap_or(true)
    })
}
