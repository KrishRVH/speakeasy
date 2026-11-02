use anyhow::{Context as _, anyhow};
use gpui::{
    AccessibleAction, AnyWindowHandle, App, AppContext, AsyncApp, Context, Render,
    StatefulInteractiveElement, Task, WeakEntity, Window, WindowHandle,
};
use raw_window_handle::HasWindowHandle;
use speakeasy_platform::NativeWindow;
use tokio::sync::watch;

pub(crate) trait EntityUpdate<T> {
    fn update_if_alive<C: AppContext>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut T, &mut Context<'_, T>),
    );
}

impl<T: 'static> EntityUpdate<T> for WeakEntity<T> {
    fn update_if_alive<C: AppContext>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut T, &mut Context<'_, T>),
    ) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A released entity has no state left to update"
        )]
        let _ = self.update(cx, update);
    }
}

pub(crate) trait AriaDisabled: StatefulInteractiveElement {
    fn aria_disabled(self, disabled: bool) -> Self {
        if !disabled {
            return self;
        }
        self.a11y_synthetic_children(|subtree| {
            let node = subtree.parent_node();
            node.set_disabled();
            node.remove_action(AccessibleAction::Click);
            node.remove_action(AccessibleAction::Focus);
        })
    }
}

impl<E: StatefulInteractiveElement> AriaDisabled for E {}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presence {
    Hidden,
    Passive,
    Raised,
}

pub(crate) fn defer_presence(window: AnyWindowHandle, presence: Presence, cx: &App) -> Task<()> {
    cx.spawn(async move |cx| {
        present(window, presence, cx);
    })
}

pub(crate) fn present(window: AnyWindowHandle, presence: Presence, cx: &mut AsyncApp) -> bool {
    let presented = set_presence(window, presence, cx);
    if presented && presence == Presence::Raised {
        cx.update(|cx| cx.activate(true));
    }
    presented
}

#[cfg(target_os = "linux")]
fn set_presence(window: AnyWindowHandle, presence: Presence, cx: &mut AsyncApp) -> bool {
    cx.update_window(window, |_, window, _| {
        window.set_window_visible(presence != Presence::Hidden);
        if presence == Presence::Raised {
            window.activate_window();
        }
    })
    .is_ok()
}

#[cfg(not(target_os = "linux"))]
fn set_presence(window: AnyWindowHandle, presence: Presence, cx: &mut AsyncApp) -> bool {
    let Ok(Ok(native)) =
        cx.update_window(window, |_, window, _| native_window(window, "Speakeasy"))
    else {
        return false;
    };
    match presence {
        Presence::Hidden => native.set_window_visible(false),
        Presence::Passive => native.set_pill_visible(true),
        Presence::Raised => native.set_window_visible(true),
    }
    true
}
