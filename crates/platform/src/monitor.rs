use std::sync::{Mutex, MutexGuard, PoisonError};

struct MonitorState<T> {
    stopping: bool,
    handle: Option<T>,
}

pub(crate) struct MonitorControl<T> {
    #[expect(
        clippy::disallowed_types,
        reason = "Stop and native handle publication must be atomic while the native loop cannot receive owner messages"
    )]
    state: Mutex<MonitorState<T>>,
}

impl<T> Default for MonitorControl<T> {
    fn default() -> Self {
        Self {
            state: MonitorState {
                stopping: false,
                handle: None,
            }
            .into(),
        }
    }
}

impl<T> MonitorControl<T> {
    #[expect(
        clippy::significant_drop_tightening,
        reason = "Readiness must publish under the same lock as native initialization so stop cannot interleave"
    )]
    pub(crate) fn start(&self, handle: T, ready: impl FnOnce()) -> bool {
        let mut state = self.state();
        if state.stopping {
            return false;
        }
        state.handle = Some(handle);
        ready();
        true
    }

    pub(crate) fn request_stop(&self, wake: impl FnOnce(&T)) {
        let mut state = self.state();
        state.stopping = true;
        if let Some(handle) = &state.handle {
            wake(handle);
        }
    }

    pub(crate) fn stopping(&self) -> bool {
        self.state().stopping
    }

    pub(crate) fn retire(&self) {
        self.state().handle = None;
    }

    fn state(&self) -> MutexGuard<'_, MonitorState<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::MonitorControl;

    #[test]
    fn monitor_stop_before_start_never_publishes_ready() {
        let control = MonitorControl::default();
        let mut woke = false;
        control.request_stop(|_| woke = true);
        let mut ready = false;
        assert!(!control.start(42, || ready = true));
        control.retire();
        assert!(!woke && !ready);
        assert!(control.stopping());
        assert!(
            !control.start(43, || {}),
            "Retirement made a stop-before-start monitor ready"
        );
    }

    #[test]
    fn monitor_stop_wakes_only_its_published_handle() {
        let control = MonitorControl::default();
        let mut ready = false;
        assert!(control.start(42, || ready = true));
        assert!(ready);
        let mut woken = None;
        control.request_stop(|handle| woken = Some(*handle));
        assert_eq!(woken, Some(42));
        control.retire();
        woken = None;
        control.request_stop(|handle| woken = Some(*handle));
        assert_eq!(woken, None);
        assert!(!control.start(43, || {}));
    }
}
