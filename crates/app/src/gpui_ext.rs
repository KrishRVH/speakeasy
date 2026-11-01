use anyhow::{Context as _, anyhow};
use gpui::{
    AnyWindowHandle, App, AppContext, AsyncApp, Context, Flatten, Render, Task, WeakEntity, Window,
    WindowHandle,
};
use raw_window_handle::HasWindowHandle;
use speakeasy_platform::NativeWindow;
use tokio::sync::watch;

pub(crate) trait EntityUpdate<T> {
    fn update_if_alive<C>(&self, cx: &mut C, update: impl FnOnce(&mut T, &mut Context<'_, T>))
    where
        C: AppContext,
        anyhow::Result<C::Result<()>>: Flatten<()>;
}

impl<T: 'static> EntityUpdate<T> for WeakEntity<T> {
    fn update_if_alive<C>(&self, cx: &mut C, update: impl FnOnce(&mut T, &mut Context<'_, T>))
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
        update: impl FnOnce(&mut V, &mut Window, &mut Context<'_, V>),
    );
}

impl<V: Render> WindowUpdate<V> for WindowHandle<V> {
    fn update_if_open<C: AppContext>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut V, &mut Window, &mut Context<'_, V>),
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

pub(crate) fn native_window(window: &Window, name: &str) -> anyhow::Result<NativeWindow> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow!("Cannot access {name} window: {error}"))?;
    NativeWindow::new(handle).with_context(|| format!("Cannot access {name} window"))
}

pub(crate) fn follow<V: 'static, T: 'static, U: 'static>(
    mut updates: watch::Receiver<T>,
    cx: &Context<'_, V>,
    select: impl Fn(&T) -> U + 'static,
    mut apply: impl FnMut(&mut V, U, &mut Context<'_, V>) + 'static,
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
