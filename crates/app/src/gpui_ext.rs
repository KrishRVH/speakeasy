//! GPUI conveniences: updates whose only failure is that their target no longer exists, and native
//! windows for platform calls.
//!
//! Each update closure returns `()`, so these helpers can never discard an error the closure itself
//! produced.

use anyhow::{Context as _, anyhow};
use gpui::{App, AppContext, AsyncApp, Context, Flatten, Render, WeakEntity, Window, WindowHandle};
use raw_window_handle::HasWindowHandle;
use speakeasy_platform::NativeWindow;

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
