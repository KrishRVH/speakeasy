//! Service ownership across enable, pause, and quit, and the epoch that tells a late completion
//! whether a newer request superseded it. A stopping owner is retained until acknowledgement; only
//! the latest restart queued behind it runs.

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

/// Where the service owners are between enable, pause, and quit, and the epoch of the latest
/// request. Generic so tests can stand in for the native owners and the restart they queue.
pub(super) struct Lifecycle<Owners, Restart> {
    state: State<Owners, Restart>,
    epoch: ConfigEpoch,
}

#[derive(Default)]
enum State<Owners, Restart> {
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

impl<Owners, Restart> Default for Lifecycle<Owners, Restart> {
    fn default() -> Self {
        Self {
            state: State::Disabled,
            epoch: ConfigEpoch::default(),
        }
    }
}

impl<Owners, Restart> Lifecycle<Owners, Restart> {
    /// The epoch of the latest request, which a save records when it is requested.
    pub(super) const fn epoch(&self) -> ConfigEpoch {
        self.epoch
    }

    /// Whether dictation is on: owners run, as `live` judges them, or a resume is checking the
    /// saved settings.
    pub(super) fn running(&self, live: impl FnOnce(&Owners) -> bool) -> bool {
        match &self.state {
            State::Running(owners) => live(owners),
            State::Validating(epoch) => *epoch == self.epoch,
            State::Disabled | State::Stopping { .. } | State::Quitting(_) => false,
        }
    }

    /// Whether a save requested at `saved_at` may enable dictation. A Pause after the Save wins:
    /// only a save whose request no later one superseded may enable it, unless dictation is on
    /// anyway or a restart waits for retiring owners.
    pub(super) fn save_may_enable(
        &self,
        saved_at: ConfigEpoch,
        live: impl FnOnce(&Owners) -> bool,
    ) -> bool {
        saved_at == self.epoch || self.running(live) || self.has_pending()
    }

    /// Advances the epoch for new settings and returns true, unless Quit has begun.
    pub(super) fn reconfigure(&mut self) -> bool {
        if self.quitting() {
            return false;
        }
        self.epoch = self.epoch.next();
        true
    }

    /// Starts checking the saved settings under a new epoch and returns it, unless owners exist or
    /// Quit has begun.
    pub(super) fn validate(&mut self) -> Option<ConfigEpoch> {
        if !matches!(self.state, State::Disabled | State::Validating(_)) {
            return None;
        }
        self.epoch = self.epoch.next();
        self.state = State::Validating(self.epoch);
        Some(self.epoch)
    }

    /// Ends the check started at `epoch` and returns true, unless a later request superseded it.
    pub(super) fn validated(&mut self, epoch: ConfigEpoch) -> bool {
        if !matches!(self.state, State::Validating(current) if current == epoch) {
            return false;
        }
        self.state = State::Disabled;
        true
    }

    /// Starts owners with `launch` and returns whether it ran, which it does only while no owners
    /// exist and Quit has not begun. The settings `launch` starts from are newer than any check
    /// still running, so that check ends even if `launch` fails.
    pub(super) fn start<E>(
        &mut self,
        launch: impl FnOnce() -> Result<Owners, E>,
    ) -> Result<bool, E> {
        if !matches!(self.state, State::Disabled | State::Validating(_)) {
            return Ok(false);
        }
        self.state = State::Disabled;
        self.state = State::Running(launch()?);
        Ok(true)
    }

    pub(super) const fn active(&self) -> Option<&Owners> {
        match &self.state {
            State::Running(owners) => Some(owners),
            State::Disabled
            | State::Validating(_)
            | State::Stopping { .. }
            | State::Quitting(_) => None,
        }
    }

    pub(super) const fn retiring(&self) -> Option<&Owners> {
        match &self.state {
            State::Stopping { owners, .. } | State::Quitting(Some(owners)) => Some(owners),
            State::Disabled | State::Validating(_) | State::Running(_) | State::Quitting(None) => {
                None
            },
        }
    }

    /// Whether owners are stopping or Quit has begun; either way, no resume may start owners.
    pub(super) const fn pausing(&self) -> bool {
        matches!(self.state, State::Stopping { .. } | State::Quitting(_))
    }

    pub(super) const fn quitting(&self) -> bool {
        matches!(self.state, State::Quitting(_))
    }

    /// Queues `restart` to run once retirement completes; returns whether owners are stopping to
    /// receive it.
    pub(super) fn queue(&mut self, restart: Restart) -> bool {
        if let State::Stopping { pending, .. } = &mut self.state {
            *pending = Some(restart);
            true
        } else {
            false
        }
    }

    const fn has_pending(&self) -> bool {
        matches!(
            self.state,
            State::Stopping {
                pending: Some(_),
                ..
            }
        )
    }

    /// Advances the epoch and moves live owners to Stopping, abandoning any validation or queued
    /// restart, unless Quit has begun. Returns whether retirement must start.
    pub(super) fn pause(&mut self) -> bool {
        let (state, started) = match mem::take(&mut self.state) {
            State::Running(owners) => (
                State::Stopping {
                    owners,
                    pending: None,
                },
                true,
            ),
            State::Stopping { owners, .. } => (
                State::Stopping {
                    owners,
                    pending: None,
                },
                false,
            ),
            State::Disabled | State::Validating(_) => (State::Disabled, false),
            quitting @ State::Quitting(_) => {
                self.state = quitting;
                return false;
            },
        };
        self.state = state;
        self.epoch = self.epoch.next();
        started
    }

    /// Enters the terminal Quitting state under a new epoch, keeping owners for retirement.
    /// Returns false when already quitting.
    pub(super) fn quit(&mut self) -> bool {
        let owners = match mem::take(&mut self.state) {
            State::Running(owners) | State::Stopping { owners, .. } => Some(owners),
            quitting @ State::Quitting(_) => {
                self.state = quitting;
                return false;
            },
            State::Disabled | State::Validating(_) => None,
        };
        self.state = State::Quitting(owners);
        self.epoch = self.epoch.next();
        true
    }

    /// Releases retired owners and returns the restart queued behind them.
    pub(super) fn retired(&mut self) -> Option<Restart> {
        match &mut self.state {
            State::Stopping { pending, .. } => {
                let pending = pending.take();
                self.state = State::Disabled;
                pending
            },
            State::Quitting(owners) => {
                *owners = None;
                None
            },
            State::Disabled | State::Validating(_) | State::Running(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::{Lifecycle, State};

    /// A lifecycle whose owners are live, as `start` leaves it.
    fn live<Owners, Restart>(owners: Owners) -> Lifecycle<Owners, Restart> {
        let mut state = Lifecycle::default();
        assert_eq!(state.start(|| Ok::<_, ()>(owners)), Ok(true));
        state
    }

    #[test]
    fn validating_and_starting_never_discard_live_or_retiring_owners() {
        struct Owner(mpsc::Sender<()>);

        impl Drop for Owner {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
            }
        }

        let (completion, dropped) = mpsc::channel();
        let mut state = live::<_, ()>(Owner(completion.clone()));
        assert!(state.validate().is_none());
        assert_eq!(
            state.start(|| Ok::<_, ()>(Owner(completion.clone()))),
            Ok(false)
        );
        assert!(dropped.try_recv().is_err());
        assert!(state.pause());
        assert!(state.validate().is_none());
        assert_eq!(
            state.start(|| Ok::<_, ()>(Owner(completion.clone()))),
            Ok(false)
        );
        assert!(dropped.try_recv().is_err());
        state.retired();
        dropped.try_recv().unwrap();
        let checking = state.validate().unwrap();
        assert!(state.running(|_| false));
        assert!(state.quit());
        assert!(!state.running(|_| false));
        assert!(!state.validated(checking));
        assert!(state.validate().is_none());
        assert_eq!(state.start(|| Ok::<_, ()>(Owner(completion))), Ok(false));
    }

    #[test]
    fn pause_invalidates_validation_and_repeated_pause_discards_resume() {
        let mut state = Lifecycle::<(), u8>::default();
        let checking = state.validate().unwrap();
        assert!(!state.pause());
        assert!(!state.validated(checking));
        state = live(());
        assert!(state.pause());
        assert!(state.queue(1));
        assert!(state.queue(2));
        assert!(!state.pause());
        assert!(!state.has_pending());
        assert_eq!(state.retired(), None);
    }

    #[test]
    fn retirement_applies_only_latest_request_once_and_quit_is_terminal() {
        let mut state = live(());
        assert!(state.pause());
        assert!(state.queue(1));
        assert!(state.queue(2));
        assert_eq!(state.retired(), Some(2));
        assert_eq!(state.retired(), None);
        state = live(());
        assert!(state.pause());
        assert!(state.queue(3));
        assert!(state.quit());
        assert!(!state.quit());
        assert!(!state.queue(4));
        assert!(!state.pause());
        assert!(!state.reconfigure());
        assert_eq!(state.retired(), None);
        assert!(state.quitting());
        assert!(state.active().is_none());
        assert!(state.retiring().is_none());
    }

    #[test]
    fn a_pause_after_save_keeps_dictation_off_unless_resumed() {
        let mut state = live::<_, ()>(());
        let saved_at = state.epoch();
        assert!(state.pause());
        assert!(
            !state.save_may_enable(saved_at, |()| true),
            "A save requested before Pause would restart the owners Pause is retiring"
        );
        assert_eq!(state.retired(), None);
        assert!(
            !state.save_may_enable(saved_at, |()| true),
            "A save requested before Pause would enable dictation after it"
        );
        state.validate().unwrap();
        assert!(
            state.save_may_enable(saved_at, |()| true),
            "A deliberate Resume after Pause still receives the durable settings"
        );
        state = Lifecycle::default();
        state.validate().unwrap();
        let saved_at = state.epoch();
        assert!(!state.pause());
        assert!(!state.save_may_enable(saved_at, |()| true));
    }

    #[test]
    fn a_save_during_a_resume_check_supersedes_the_check() {
        // The save's settings start owners; the check that finishes afterward is stale.
        let mut state = Lifecycle::<u8, ()>::default();
        let checking = state.validate().unwrap();
        assert!(state.save_may_enable(state.epoch(), |_| false));
        assert!(state.reconfigure());
        assert!(!state.queue(()));
        assert_eq!(state.start(|| Ok::<_, ()>(1)), Ok(true));
        assert!(!state.validated(checking));
        assert_eq!(state.active(), Some(&1));
        // Owners that fail to start still end the check, which would apply older settings.
        let mut state = Lifecycle::<u8, ()>::default();
        let checking = state.validate().unwrap();
        assert!(state.reconfigure());
        assert_eq!(state.start(|| Err::<u8, _>(())), Err(()));
        assert!(
            !state.validated(checking),
            "A check outlived newer settings whose owners failed to start"
        );
        assert!(matches!(state.state, State::Disabled));
    }
}
