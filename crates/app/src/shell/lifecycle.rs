//! Service ownership across enable, pause, and quit. A stopping owner is retained until
//! acknowledgement; only the latest restart queued behind it runs.

use std::mem;

/// Advances whenever the requested service state changes, so a late completion can tell that it is
/// stale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ConfigEpoch(u64);

impl ConfigEpoch {
    pub(super) const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Where the service owners are between enable, pause, and quit. Generic so tests can stand in for
/// the native owners and the restart they queue.
#[derive(Default)]
pub(super) enum Lifecycle<Owners, Restart> {
    /// Dictation is off; no owners exist or are starting.
    #[default]
    Disabled,
    /// Saved settings are checked off the UI thread before any owner exists; only the check for
    /// this epoch may start owners.
    Validating(ConfigEpoch),
    /// Owners are live.
    Running(Owners),
    /// Owners are retiring; `pending` restarts the service once they acknowledge.
    Stopping {
        owners: Owners,
        pending: Option<Restart>,
    },
    /// Terminal. Holds owners until they acknowledge retirement; `None` once none remain.
    Quitting(Option<Owners>),
}

impl<Owners, Restart> Lifecycle<Owners, Restart> {
    /// Starts validating for `epoch` and returns true, unless live or retiring owners exist.
    pub(super) fn validate(&mut self, epoch: ConfigEpoch) -> bool {
        if matches!(self, Self::Disabled | Self::Validating(_)) {
            *self = Self::Validating(epoch);
            true
        } else {
            false
        }
    }

    pub(super) fn active(&self) -> Option<&Owners> {
        match self {
            Self::Running(owners) => Some(owners),
            Self::Disabled | Self::Validating(_) | Self::Stopping { .. } | Self::Quitting(_) => {
                None
            },
        }
    }

    pub(super) fn retiring(&self) -> Option<&Owners> {
        match self {
            Self::Stopping { owners, .. } | Self::Quitting(Some(owners)) => Some(owners),
            Self::Disabled | Self::Validating(_) | Self::Running(_) | Self::Quitting(None) => None,
        }
    }

    /// Whether the UI shows Pausing: owners are stopping, or Quit has begun. Either way, no resume
    /// may start owners.
    pub(super) fn pausing(&self) -> bool {
        matches!(self, Self::Stopping { .. } | Self::Quitting(_))
    }

    pub(super) fn quitting(&self) -> bool {
        matches!(self, Self::Quitting(_))
    }

    pub(super) fn validating(&self, epoch: ConfigEpoch) -> bool {
        matches!(self, Self::Validating(current) if *current == epoch)
    }

    /// Queues `restart` to run once retirement completes; returns whether owners are stopping to
    /// receive it.
    pub(super) fn queue(&mut self, restart: Restart) -> bool {
        if let Self::Stopping { pending, .. } = self {
            *pending = Some(restart);
            true
        } else {
            false
        }
    }

    pub(super) fn has_pending(&self) -> bool {
        matches!(
            self,
            Self::Stopping {
                pending: Some(_),
                ..
            }
        )
    }

    /// Moves live owners to Stopping, abandoning any validation or queued restart. Returns whether
    /// retirement must start.
    pub(super) fn pause(&mut self) -> bool {
        let (owners, started) = match mem::take(self) {
            Self::Running(owners) => (owners, true),
            Self::Stopping { owners, .. } => (owners, false),
            quitting @ Self::Quitting(_) => {
                *self = quitting;
                return false;
            },
            Self::Disabled | Self::Validating(_) => return false,
        };
        *self = Self::Stopping {
            owners,
            pending: None,
        };
        started
    }

    /// Enters the terminal Quitting state, keeping owners for retirement. Returns false when
    /// already quitting.
    pub(super) fn quit(&mut self) -> bool {
        let owners = match mem::take(self) {
            Self::Running(owners) | Self::Stopping { owners, .. } => Some(owners),
            quitting @ Self::Quitting(_) => {
                *self = quitting;
                return false;
            },
            Self::Disabled | Self::Validating(_) => None,
        };
        *self = Self::Quitting(owners);
        true
    }

    /// Releases retired owners and returns the restart queued behind them.
    pub(super) fn retired(&mut self) -> Option<Restart> {
        match self {
            Self::Stopping { pending, .. } => {
                let pending = pending.take();
                *self = Self::Disabled;
                pending
            },
            Self::Quitting(owners) => {
                *owners = None;
                None
            },
            Self::Disabled | Self::Validating(_) | Self::Running(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::{ConfigEpoch, Lifecycle};

    #[test]
    fn validating_never_discards_live_or_retiring_owners() {
        struct Owner(mpsc::Sender<()>);

        impl Drop for Owner {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
            }
        }

        let (completion, dropped) = mpsc::channel();
        let mut state = Lifecycle::<_, ()>::Running(Owner(completion));
        assert!(!state.validate(ConfigEpoch(1)));
        assert!(dropped.try_recv().is_err());
        assert!(state.pause());
        assert!(!state.validate(ConfigEpoch(2)));
        assert!(dropped.try_recv().is_err());
        state.retired();
        dropped.try_recv().unwrap();
        assert!(state.validate(ConfigEpoch(3)));
        assert!(state.validating(ConfigEpoch(3)));
        assert!(state.quit());
        assert!(!state.validate(ConfigEpoch(4)));
    }

    #[test]
    fn pause_invalidates_validation_and_repeated_pause_discards_resume() {
        let mut state = Lifecycle::<(), u8>::Validating(ConfigEpoch(1));
        assert!(!state.pause());
        assert!(!state.validating(ConfigEpoch(1)));
        state = Lifecycle::Running(());
        assert!(state.pause());
        assert!(state.queue(1));
        assert!(state.queue(2));
        assert!(!state.pause());
        assert!(!state.has_pending());
        assert_eq!(state.retired(), None);
    }

    #[test]
    fn retirement_applies_only_latest_request_once_and_quit_is_terminal() {
        let mut state = Lifecycle::Running(());
        assert!(state.pause());
        assert!(state.queue(1));
        assert!(state.queue(2));
        assert_eq!(state.retired(), Some(2));
        assert_eq!(state.retired(), None);
        state = Lifecycle::Running(());
        assert!(state.pause());
        assert!(state.queue(3));
        assert!(state.quit());
        assert!(!state.quit());
        assert!(!state.queue(4));
        assert!(!state.pause());
        assert_eq!(state.retired(), None);
        assert!(state.quitting());
        assert!(state.active().is_none());
        assert!(state.retiring().is_none());
    }
}
