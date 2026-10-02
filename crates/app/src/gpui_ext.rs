//! GPUI conveniences: updates whose only failure is that their target no longer exists, native
//! handles for platform calls, and theme colors as GPUI fills.
//!
//! Each update closure returns `()`, so these helpers can never discard an error the closure itself
//! produced.

use gpui::{App, AppContext, AsyncApp, Context, Flatten, Render, WeakEntity, Window, WindowHandle};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

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

/// The native handle of `window`; `name` labels the error people see.
pub(crate) fn raw_handle(window: &Window, name: &str) -> anyhow::Result<RawWindowHandle> {
    HasWindowHandle::window_handle(window)
        .map(|handle| handle.as_raw())
        .map_err(|error| anyhow::anyhow!("Cannot access {name} window: {error}"))
}

/// A `0xRRGGBB` color with an opacity, for GPUI fills and borders.
pub(crate) fn alpha(color: u32, opacity: f32) -> gpui::Rgba {
    gpui::Rgba {
        a: opacity,
        ..gpui::rgb(color)
    }
}
