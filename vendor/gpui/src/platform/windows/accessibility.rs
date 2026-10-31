//! Per-window UI Automation keeps native reentrancy outside GPUI state borrows.

use super::*;

pub(in crate::platform::windows) struct State {
    pub(in crate::platform::windows) adapter: accesskit_windows::Adapter,
    activation: Activation,
    invalidate: Box<dyn Fn() + Send>,
    redacted: bool,
    requested_visible: bool,
}

impl State {
    pub(in crate::platform::windows) fn new(hwnd: HWND, callbacks: crate::A11yCallbacks) -> Self {
        // SAFETY: this query has no preconditions and returns only the foreground HWND.
        let focused = unsafe { GetForegroundWindow() } == hwnd;
        // SAFETY: hwnd is the live native window retained by the adapter owner.
        let (shown, minimized) =
            unsafe { (IsWindowVisible(hwnd).as_bool(), IsIconic(hwnd).as_bool()) };
        Self {
            adapter: accesskit_windows::Adapter::new(
                accesskit_windows::HWND(hwnd.0),
                focused,
                Actions(callbacks.action),
            ),
            activation: Activation(callbacks.activation),
            invalidate: callbacks.invalidate,
            redacted: !shown || minimized,
            requested_visible: shown,
        }
    }
}

struct Activation(Box<dyn Fn() -> Option<accesskit::TreeUpdate> + Send>);

impl accesskit::ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        (self.0)()
    }
}

struct Actions(Box<dyn Fn(accesskit::ActionRequest) + Send>);

impl accesskit::ActionHandler for Actions {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        (self.0)(request);
    }
}

impl WindowsWindowInner {
    fn native_accessibility_visible(&self) -> bool {
        // SAFETY: this live platform window owns hwnd and both queries are read-only.
        unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() }
    }

    pub(in crate::platform::windows) fn accessibility_visible(&self) -> bool {
        self.native_accessibility_visible()
            && self
                .a11y
                .borrow()
                .as_ref()
                .is_some_and(|state| !state.redacted)
    }

    pub(in crate::platform::windows) fn request_accessibility_visibility(&self, visible: bool) {
        if let Some(state) = self.a11y.borrow_mut().as_mut() {
            state.requested_visible = visible;
        }
        if !visible {
            self.redact_accessibility();
        }
    }

    pub(in crate::platform::windows) fn reconcile_accessibility_visibility(
        &self,
        flags: SET_WINDOW_POS_FLAGS,
    ) {
        let requested = self.a11y.borrow_mut().as_mut().is_some_and(|state| {
            if flags.contains(SWP_HIDEWINDOW) {
                state.requested_visible = false;
            }
            if flags.contains(SWP_SHOWWINDOW) {
                state.requested_visible = true;
            }
            state.requested_visible
        });
        if !requested || !self.native_accessibility_visible() {
            let exposed = self
                .a11y
                .borrow()
                .as_ref()
                .is_some_and(|state| !state.redacted);
            if exposed {
                self.redact_accessibility();
            }
            return;
        }
        let republish = self
            .a11y
            .borrow_mut()
            .as_mut()
            .is_some_and(|state| std::mem::replace(&mut state.redacted, false));
        if republish {
            // WINDOWPOSCHANGED observes the committed visible style. Messages posted
            // from SHOWWINDOW can run reentrantly before that transition completes.
            // SAFETY: hwnd is owned by the live window receiving WINDOWPOSCHANGED.
            unsafe {
                PostMessageW(
                    Some(self.hwnd),
                    WM_GPUI_FORCE_UPDATE_WINDOW,
                    WPARAM(0),
                    LPARAM(0),
                )
                .log_err();
            }
        }
    }

    pub(in crate::platform::windows) fn accessibility_object(
        &self,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<isize> {
        let result = {
            let mut state = self.a11y.borrow_mut();
            let state = state.as_mut()?;
            state.adapter.handle_wm_getobject(
                accesskit_windows::WPARAM(wparam.0),
                accesskit_windows::LPARAM(lparam.0),
                &mut state.activation,
            )?
        };
        // UIA may synchronously send another WM_GETOBJECT while returning this provider.
        let result: accesskit_windows::LRESULT = result.into();
        Some(result.0)
    }

    pub(in crate::platform::windows) fn redact_accessibility(&self) {
        let events = self.a11y.borrow_mut().as_mut().and_then(|state| {
            state.redacted = true;
            (state.invalidate)();
            state
                .adapter
                .update_if_active(crate::platform::empty_accessibility_tree)
        });
        // Raising events may synchronously query the same provider again.
        if let Some(events) = events {
            events.raise();
        }
    }

    pub(in crate::platform::windows) fn disconnect_accessibility(&self) {
        // Redaction drains native tree readers while hwnd is still valid. Readers that
        // already upgraded their weak context can then see only a boundless empty root.
        let state = self.a11y.borrow_mut().take();
        if let Some(mut state) = state {
            (state.invalidate)();
            if let Some(events) = state
                .adapter
                .update_if_active(crate::platform::empty_accessibility_tree)
            {
                events.raise();
            }
        }
    }
}
