//! GPUI conveniences: updates whose only failure is that their target no longer exists, views that
//! follow a watch channel, and native windows for platform calls, now or deferred past GPUI's
//! window borrow.
//!
//! The `update_if_*` closures return `()`, so those helpers can never discard an error the closure
//! itself produced.

use anyhow::{Context as _, anyhow};
use gpui::{
    AnyWindowHandle, App, AppContext, AsyncApp, Context, Flatten, Render, Task, WeakEntity, Window,
    WindowHandle,
};
use raw_window_handle::HasWindowHandle;
use speakeasy_platform::NativeWindow;
use tokio::sync::watch;

pub(crate) trait EntityUpdate<T> {
    /// The `Flatten` bound mirrors `WeakEntity::update`, so both `App` and `AsyncApp` qualify.
    fn update_if_alive<C>(&self, cx: &mut C, update: impl FnOnce(&mut T, &mut Context<T>))
    where
        C: AppContext,
        anyhow::Result<C::Result<()>>: Flatten<()>;
}

impl<T: 'static> EntityUpdate<T> for WeakEntity<T> {
    fn update_if_alive<C>(&self, cx: &mut C, update: impl FnOnce(&mut T, &mut Context<T>))
    where
        C: AppContext,
        anyhow::Result<C::Result<()>>: Flatten<()>,
    {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A released entity has no state left to update"
        )]
        let _ = self.update(cx, update);
    }
}

pub(crate) trait WindowUpdate<V> {
    fn update_if_open<C: AppContext>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut V, &mut Window, &mut Context<V>),
    );
}

impl<V: Render> WindowUpdate<V> for WindowHandle<V> {
    fn update_if_open<C: AppContext>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut V, &mut Window, &mut Context<V>),
    ) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A closed window has no view left to update"
        )]
        let _ = self.update(cx, update);
    }
}

pub(crate) trait AppUpdate {
    fn update_if_running(&self, update: impl FnOnce(&mut App));
}

impl AppUpdate for AsyncApp {
    fn update_if_running(&self, update: impl FnOnce(&mut App)) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A released app has already disposed of every view an update could reach"
        )]
        let _ = self.update(update);
    }
}

/// The native window behind `window`, for calls made once GPUI's borrow of it ends; `name` labels
/// the error people see.
pub(crate) fn native_window(window: &Window, name: &str) -> anyhow::Result<NativeWindow> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow!("Cannot access {name} window: {error}"))?;
    NativeWindow::new(handle).with_context(|| format!("Cannot access {name} window"))
}

/// Applies the receiver's current value to the view, then each later one, while the view and the
/// publisher live. `select` copies what `apply` needs while the watch lock is held; the lock is
/// released before the view updates, so a publisher on another thread never waits on GPUI.
pub(crate) fn follow<V: 'static, T: 'static, U: 'static>(
    mut updates: watch::Receiver<T>,
    cx: &Context<V>,
    select: impl Fn(&T) -> U + 'static,
    mut apply: impl FnMut(&mut V, U, &mut Context<V>) + 'static,
) -> Task<()> {
    updates.mark_changed();
    cx.spawn(async move |view, cx| {
        while updates.changed().await.is_ok() {
            let value = select(&updates.borrow_and_update());
            if view.update(cx, |view, cx| apply(view, value, cx)).is_err() {
                break;
            }
        }
    })
}

/// Runs `native` on `window`'s native window in a task, after GPUI releases the current window
/// borrow: native show can synchronously request a GPUI frame. A closed window, or one whose handle
/// cannot be read, skips the call. Dropping or replacing the returned task cancels a call that has
/// not run, so a caller keeps one slot per window and a newer request supersedes an older one.
pub(crate) fn defer_native(
    window: AnyWindowHandle,
    name: &'static str,
    cx: &App,
    native: impl FnOnce(&NativeWindow, &AsyncApp) + 'static,
) -> Task<()> {
    cx.spawn(async move |cx| {
        if let Ok(Ok(handle)) = cx.update_window(window, |_, window, _| native_window(window, name))
        {
            native(&handle, cx);
        }
    })
}
