//! App-owned Quit: drain native work and accepted saves, then ask GPUI to exit.
//!
//! The drain has no timeout; a stuck owner keeps Quitting shown while the UI keeps rendering.
//! GPUI's own forced shutdown skips the drain and falls back on each owner's synchronous `Drop`.

use std::time::Duration;

use gpui::{App, AsyncApp, WindowHandle};

use super::{Services, settings::Settings};
use crate::gpui_ext::WindowUpdate;

const DRAIN_POLL: Duration = Duration::from_millis(10);

// Clippy's `allow-*-in-tests` settings recognize only a bare `cfg(test)`, so the platform gate is
// a separate attribute.
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
pub(super) fn request_install(installer: crate::updates::Installer, cx: &mut App) {
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
    let Some(stopped) = services.begin_quit(
        #[cfg(target_os = "windows")]
        installer,
    ) else {
        return;
    };
    let settings = services.settings;
    let shows = services.shows;
    let task = cx.spawn(async move |cx| {
        if let Some(shows) = shows {
            shows.update_if_open(cx, |_, window, _| window.remove_window());
        }

        // Touch Settings only inside this task: the action that requested Quit may still be
        // updating its window, where a nested `update_if_open` silently does nothing.
        if let Some(settings) = settings {
            settings.update_if_open(cx, |view, _, _| view.cancel_background_work());
        }
        stopped.await;
        while !drained(settings, cx) {
            cx.background_executor().timer(DRAIN_POLL).await;
        }
        cx.update(|cx| {
            cx.global_mut::<Services>().finish_quit();
            cx.quit();
        });
    });
    cx.global_mut::<Services>().quit = Some(task);
}

/// Whether Settings has written every accepted save and stopped its background work.
///
/// A queued draft counts: `SaveQueue::finish` starts its write when the in-flight one lands. Quit
/// asks GPUI to exit only after this.
fn drained(settings: Option<WindowHandle<Settings>>, cx: &mut AsyncApp) -> bool {
    settings.is_none_or(|settings| {
        settings
            .update(cx, |view, _, _| view.drained())
            // A closed window is drained: disposing Settings waits for its in-flight write and
            // drops any queued draft.
            .unwrap_or(true)
    })
}
