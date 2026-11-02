//! Speakeasy patch: UI Automation follows committed window visibility and ends before the HWND.

use super::*;

impl WindowsWindowInner {
    pub(crate) fn accessibility_exposed(&self) -> bool {
        self.visibility().is_visible()
            && self
                .state
                .a11y
                .borrow()
                .as_ref()
                .is_some_and(|a11y| !a11y.redacted)
    }

    pub(crate) fn request_accessibility_visibility(&self, visible: bool) {
        if let Some(a11y) = self.state.a11y.borrow_mut().as_mut() {
            a11y.requested_visible = visible;
        }
        if !visible {
            self.redact_accessibility();
        }
    }

    pub(crate) fn handle_window_pos_changed_msg(
        &self,
        handle: HWND,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<isize> {
        let flags = unsafe { (lparam.0 as *const WINDOWPOS).as_ref() }
            .map(|position| position.flags)
            .unwrap_or_default();
        // Default processing delivers WM_SIZE and WM_MOVE before visibility is reconciled.
        let result = unsafe { DefWindowProcW(handle, WM_WINDOWPOSCHANGED, wparam, lparam) };
        self.reconcile_accessibility_visibility(flags);
        Some(result.0)
    }

    fn reconcile_accessibility_visibility(&self, flags: SET_WINDOW_POS_FLAGS) {
        let requested = self.state.a11y.borrow_mut().as_mut().is_some_and(|a11y| {
            if flags.contains(SWP_HIDEWINDOW) {
                a11y.requested_visible = false;
            }
            if flags.contains(SWP_SHOWWINDOW) {
                a11y.requested_visible = true;
            }
            a11y.requested_visible
        });
        if !requested || !self.visibility().is_visible() {
            let exposed = self
                .state
                .a11y
                .borrow()
                .as_ref()
                .is_some_and(|a11y| !a11y.redacted);
            if exposed {
                self.redact_accessibility();
            }
            return;
        }
        let republish = self
            .state
            .a11y
            .borrow_mut()
            .as_mut()
            .is_some_and(|a11y| std::mem::replace(&mut a11y.redacted, false));
        if republish {
            // WINDOWPOSCHANGED observes the committed visible style. Messages posted while
            // WM_SHOWWINDOW is handled can run before that transition completes.
            unsafe {
                PostMessageW(
                    Some(self.hwnd),
                    WM_GPUI_FORCE_UPDATE_WINDOW,
                    WPARAM(self.validation_number),
                    LPARAM(0),
                )
            }
            .log_err();
        }
    }

    pub(crate) fn redact_accessibility(&self) {
        let events = self.state.a11y.borrow_mut().as_mut().and_then(|a11y| {
            a11y.redacted = true;
            a11y.adapter.update_if_active(empty_tree)
        });
        // Raising events may send a nested `WM_GETOBJECT` to this window.
        if let Some(events) = events {
            events.raise();
        }
    }

    pub(crate) fn disconnect_accessibility(&self) {
        let a11y = self.state.a11y.borrow_mut().take();
        if let Some(mut a11y) = a11y {
            // Publishing waits for providers that are reading the tree while the HWND is
            // still valid. Providers that keep the context afterwards see only an empty root.
            if let Some(events) = a11y.adapter.update_if_active(empty_tree) {
                events.raise();
            }
        }
    }
}

/// Keeps GPUI's root node id, so the root provider survives redaction.
pub(crate) fn empty_tree() -> accesskit::TreeUpdate {
    let root = accesskit::NodeId(0);
    accesskit::TreeUpdate {
        nodes: vec![(root, accesskit::Node::new(accesskit::Role::Window))],
        tree: Some(accesskit::Tree::new(root)),
        tree_id: accesskit::TreeId::ROOT,
        focus: root,
    }
}
