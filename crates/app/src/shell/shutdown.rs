//! All application-owned Quit routes drain native work and requested saves while the foreground
//! executor continues rendering. Cleanup has no timeout: audio driver teardown, a microphone scan,
//! process reaping, insertion, setup, and saves must acknowledge. A stuck native owner keeps
//! Quitting visible and the UI responsive; forced native termination retains synchronous Drop as
//! the emergency fallback.

use std::time::Duration;

use gpui::{App, AsyncApp, Timer, WindowHandle};

use super::{Services, settings::Settings};
use crate::gpui_ext::{AppUpdate, WindowUpdate};

const DRAIN_POLL: Duration = Duration::from_millis(10);

pub(crate) fn request_quit(cx: &mut App) {
    if !cx.has_global::<Services>() {
        cx.quit();
        return;
    }
    let services = cx.global_mut::<Services>();
    let Some(stopped) = services.begin_quit() else {
        return;
    };
    let settings = services.settings;
    let task = cx.spawn(async move |cx| {
        // The action that requested Quit may still hold the view; touch it only from here.
        if let Some(settings) = settings {
            settings.update_if_open(cx, |view, _, _| view.cancel_background_work());
        }
        stopped.await;
        while !drained(settings, cx) {
            Timer::after(DRAIN_POLL).await;
        }
        cx.update_if_running(|cx| {
            cx.global_mut::<Services>().finish_quit();
            cx.quit();
        });
    });
    cx.global_mut::<Services>().quit = Some(task);
}

/// Whether Settings, if still open, has no setup or save left: save tasks drain their queue, and
/// Quitting stops setup from queuing another.
fn drained(settings: Option<WindowHandle<Settings>>, cx: &mut AsyncApp) -> bool {
    settings.is_none_or(|settings| {
        settings
            .update(cx, |view, _, _| view.drain_finished())
            .unwrap_or(true)
    })
}
