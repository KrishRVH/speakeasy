//! All application-owned Quit routes drain native work and requested saves while
//! the foreground executor continues rendering. Cleanup has no timeout: audio
//! driver teardown, process reaping, insertion, setup, and saves must acknowledge.
//! A stuck native owner keeps Quitting visible and the UI responsive; forced
//! native termination retains synchronous Drop as the emergency fallback.
use super::{App, BorrowAppContext, Duration, Services, Settings, Timer};

pub(crate) fn request_quit(cx: &mut App) {
    if !cx.has_global::<Services>() {
        cx.quit();
        return;
    }
    let Some(stopped) = cx.update_global::<Services, _>(|services, _| services.begin_quit()) else {
        return;
    };
    let window = cx.global::<Services>().window;
    let task = cx.spawn(async move |cx| {
        // Defer view access until the action that requested Quit releases it.
        if let Some(window) = window {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A disposed Settings view already owns its synchronous cleanup fallback"
            )]
            let _ = window.update(cx, |view, _, _| {
                view.dialog.take();
                view.devices.take();
                view.preview.take();
                if let Some((setup, _)) = &view.setup {
                    setup.request_stop();
                }
                for (setup, _) in &view.retiring_setups {
                    setup.request_stop();
                }
            });
        }
        stopped.await;
        // Existing save tasks drain their coalesced queue; setup cannot enqueue
        // another save once Services enters Quitting. Retain views and the lock.
        loop {
            let ready = window.is_none_or(|window| {
                window
                    .update(cx, |view, _, _| view.shutdown_ready())
                    .unwrap_or(true)
            });
            if ready {
                break;
            }
            Timer::after(Duration::from_millis(10)).await;
        }
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A disposed app has already completed the native quit route"
        )]
        let _ = cx.update(|cx| {
            cx.update_global::<Services, _>(|services, _| services.finish_quit());
            cx.quit();
        });
    });
    cx.global_mut::<Services>().quit = Some(task);
}

impl Settings {
    fn shutdown_ready(&mut self) -> bool {
        if self
            .setup
            .as_ref()
            .is_some_and(|(setup, _)| setup.is_finished())
        {
            self.setup.take();
        }
        self.retiring_setups
            .retain(|(setup, _)| !setup.is_finished());
        self.setup.is_none()
            && self.retiring_setups.is_empty()
            && self.saving.is_none()
            && self.save_work.is_none()
            && self.saves.pending.is_none()
    }
}
