//! Stop coordination for a native event loop that cannot receive owner messages.
//!
//! Publishing the loop's handle and requesting stop share one lock, so a stop that arrives before
//! the loop starts stays effective and a wake never reaches a retired handle.

use std::sync::{Mutex, MutexGuard, PoisonError};

struct MonitorState<T> {
    stopping: bool,
    handle: Option<T>,
}

/// Stop state shared between a native event loop's thread and its owner.
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
    /// Publishes the loop's handle and runs `ready`, or returns `false` without publishing when a
    /// stop already arrived.
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

    /// Marks the loop stopping and wakes its published handle, if any.
    pub(crate) fn request_stop(&self, wake: impl FnOnce(&T)) {
        let mut state = self.state();
        state.stopping = true;
        if let Some(handle) = &state.handle {
            wake(handle);
        }
    }

    /// Whether a stop has been requested.
    pub(crate) fn stopping(&self) -> bool {
        self.state().stopping
    }

    /// Retires the published handle once the loop has exited, so later stops wake nothing.
    pub(crate) fn retire(&self) {
        self.state().handle = None;
    }

    fn state(&self) -> MutexGuard<'_, MonitorState<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };

    use super::MonitorControl;

    #[test]
    fn monitor_stop_before_delayed_start_never_publishes_ready() -> anyhow::Result<()> {
        let control = Arc::new(MonitorControl::default());
        let loop_control = Arc::clone(&control);
        let (initialized, entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (ready, events) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            initialized
                .send(())
                .expect("Fake monitor startup lane is open");
            released.recv().expect("Test releases the fake monitor");
            let started = loop_control.start(42, || {
                ready.try_send(()).expect("Fake readiness lane is open");
            });
            loop_control.retire();
            started
        });
        entered.recv_timeout(Duration::from_secs(2))?;
        let mut woke = false;
        control.request_stop(|_| woke = true);
        release.send(())?;
        let started = thread
            .join()
            .map_err(|_panic| anyhow::anyhow!("Fake monitor failed"))?;
        assert!(!started);
        assert!(!woke);
        assert!(control.stopping());
        assert!(events.recv().is_err());
        assert!(
            !control.start(43, || {}),
            "Retirement made a stop-before-start monitor ready"
        );
        Ok(())
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
