//! All application-owned Quit routes drain native work and requested saves while the foreground
//! executor continues rendering. Cleanup has no timeout: audio driver teardown, process reaping,
//! insertion, setup, and saves must acknowledge. A stuck native owner keeps Quitting visible and
//! the UI responsive; forced native termination retains synchronous Drop as the emergency fallback.

use std::time::Duration;

use gpui::{App, BorrowAppContext, Timer};

use super::Services;
use crate::gpui_ext::{AppUpdate, WindowUpdate};

const DRAIN_POLL: Duration = Duration::from_millis(10);

pub(crate) fn request_quit(cx: &mut App) {
    if !cx.has_global::<Services>() {
        cx.quit();
        return;
    }
    let Some(stopped) = cx.update_global::<Services, _>(|services, _| services.begin_quit()) else {
        return;
    };
    let settings = cx.global::<Services>().settings;
    let task = cx.spawn(async move |cx| {
        // The action that requested Quit may still hold the view; touch it only from here.
        if let Some(settings) = settings {
            settings.update_if_open(cx, |view, _, _| view.cancel_background_work());
        }
        stopped.await;
        // Save tasks drain their queue, and Quitting stops setup from queuing another. Views and
        // the instance lock stay alive until then.
        loop {
            let drained = settings.is_none_or(|settings| {
                settings
                    .update(cx, |view, _, _| view.drain_finished())
                    .unwrap_or(true)
            });
            if drained {
                break;
            }
            Timer::after(DRAIN_POLL).await;
        }
        cx.update_if_running(|cx| {
            cx.update_global::<Services, _>(|services, _| services.finish_quit());
            cx.quit();
        });
    });
    cx.global_mut::<Services>().quit = Some(task);
}
