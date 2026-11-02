use std::mem;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ConfigEpoch(u64);

impl ConfigEpoch {
    pub(super) const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

pub(super) struct Lifecycle<Owners, Restart> {
    state: State<Owners, Restart>,
    epoch: ConfigEpoch,
    held: bool,
}

#[derive(Default)]
enum State<Owners, Restart> {
    #[default]
    Disabled,
    Validating(ConfigEpoch),
    Running(Owners),
    Stopping {
        owners: Owners,
        pending: Option<Restart>,
    },
    Quitting(Option<Owners>),
}

impl<Owners, Restart> Default for Lifecycle<Owners, Restart> {
    fn default() -> Self {
        Self {
            state: State::Disabled,
            epoch: ConfigEpoch::default(),
            held: false,
        }
    }
}

impl<Owners, Restart> Lifecycle<Owners, Restart> {
    pub(super) const fn epoch(&self) -> ConfigEpoch {
        self.epoch
    }

    pub(super) fn is_running(&self, live: impl FnOnce(&Owners) -> bool) -> bool {
        match &self.state {
            State::Running(owners) => live(owners),
            State::Validating(epoch) => *epoch == self.epoch,
            State::Disabled | State::Stopping { .. } | State::Quitting(_) => false,
        }
    }

    pub(super) fn save_may_enable(
        &self,
        saved_at: ConfigEpoch,
        live: impl FnOnce(&Owners) -> bool,
    ) -> bool {
        (!self.held && saved_at == self.epoch) || self.is_running(live) || self.has_pending()
    }

    pub(super) fn reconfigure(&mut self) -> bool {
        if self.is_quitting() {
            return false;
        }
        self.epoch = self.epoch.next();
        true
    }

    pub(super) fn validate(&mut self) -> Option<ConfigEpoch> {
        if !self.may_start() {
            return None;
        }
        self.epoch = self.epoch.next();
        self.state = State::Validating(self.epoch);
        self.held = false;
        Some(self.epoch)
    }

    pub(super) fn validated(&mut self, epoch: ConfigEpoch) -> bool {
        if !matches!(self.state, State::Validating(current) if current == epoch) {
            return false;
        }
        self.state = State::Disabled;
        true
    }

    pub(super) fn start<E>(
        &mut self,
        launch: impl FnOnce() -> Result<Owners, E>,
    ) -> Result<bool, E> {
        if !self.may_start() {
            return Ok(false);
        }
        self.state = State::Disabled;
        let owners = launch()?;
        self.state = State::Running(owners);
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

    pub(super) const fn is_pausing(&self) -> bool {
        matches!(self.state, State::Stopping { .. } | State::Quitting(_))
    }

    pub(super) const fn is_quitting(&self) -> bool {
        matches!(self.state, State::Quitting(_))
    }

    pub(super) fn queue(&mut self, restart: Restart) -> bool {
        if let State::Stopping { pending, .. } = &mut self.state {
            *pending = Some(restart);
            self.held = false;
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

    const fn may_start(&self) -> bool {
        matches!(self.state, State::Disabled | State::Validating(_))
    }

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
        self.held = true;
        started
    }

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

    fn live<Owners, Restart>(owners: Owners) -> Lifecycle<Owners, Restart> {
        let mut lifecycle = Lifecycle::default();
        assert_eq!(lifecycle.start(|| Ok::<_, ()>(owners)), Ok(true));
        lifecycle
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
        let mut lifecycle = live::<_, ()>(Owner(completion.clone()));
        assert!(lifecycle.validate().is_none());
        assert_eq!(
            lifecycle.start(|| Ok::<_, ()>(Owner(completion.clone()))),
            Ok(false)
        );
        assert!(dropped.try_recv().is_err());
        assert!(lifecycle.pause());
        assert!(lifecycle.validate().is_none());
        assert_eq!(
            lifecycle.start(|| Ok::<_, ()>(Owner(completion.clone()))),
            Ok(false)
        );
        assert!(dropped.try_recv().is_err());
        lifecycle.retired();
        dropped.try_recv().unwrap();
        let checking = lifecycle.validate().unwrap();
        assert!(lifecycle.is_running(|_| false));
        assert!(lifecycle.quit());
        assert!(!lifecycle.is_running(|_| false));
        assert!(!lifecycle.validated(checking));
        assert!(lifecycle.validate().is_none());
        assert_eq!(
            lifecycle.start(|| Ok::<_, ()>(Owner(completion))),
            Ok(false)
        );
    }

    #[test]
    fn pause_invalidates_validation_and_repeated_pause_discards_resume() {
        let mut lifecycle = Lifecycle::<(), u8>::default();
        let checking = lifecycle.validate().unwrap();
        assert!(!lifecycle.pause());
        assert!(!lifecycle.validated(checking));
        lifecycle = live(());
        assert!(lifecycle.pause());
        assert!(lifecycle.queue(1));
        assert!(lifecycle.queue(2));
        assert!(!lifecycle.pause());
        assert!(!lifecycle.has_pending());
        assert_eq!(lifecycle.retired(), None);
    }

    #[test]
    fn retirement_applies_only_latest_request_once_and_quit_is_terminal() {
        let mut lifecycle = live(());
        assert!(lifecycle.pause());
        assert!(lifecycle.queue(1));
        assert!(lifecycle.queue(2));
        assert_eq!(lifecycle.retired(), Some(2));
        assert_eq!(lifecycle.retired(), None);
        lifecycle = live(());
        assert!(lifecycle.pause());
        assert!(lifecycle.queue(3));
        assert!(lifecycle.quit());
        assert!(!lifecycle.quit());
        assert!(!lifecycle.queue(4));
        assert!(!lifecycle.pause());
        assert!(!lifecycle.reconfigure());
        assert_eq!(lifecycle.retired(), None);
        assert!(lifecycle.is_quitting());
        assert!(lifecycle.active().is_none());
        assert!(lifecycle.retiring().is_none());
    }

    #[test]
    fn a_pause_after_save_keeps_dictation_off_unless_resumed() {
        let mut lifecycle = live::<_, ()>(());
        let saved_at = lifecycle.epoch();
        assert!(lifecycle.pause());
        assert!(
            !lifecycle.save_may_enable(saved_at, |()| true),
            "A save requested before Pause would restart the owners Pause is retiring"
        );
        assert_eq!(lifecycle.retired(), None);
        assert!(
            !lifecycle.save_may_enable(saved_at, |()| true),
            "A save requested before Pause would enable dictation after it"
        );
        assert!(
            !lifecycle.save_may_enable(lifecycle.epoch(), |()| true),
            "A setting changed while paused would resume dictation"
        );
        lifecycle.validate().unwrap();
        assert!(
            lifecycle.save_may_enable(saved_at, |()| true),
            "A deliberate Resume after Pause still receives the durable settings"
        );
        lifecycle = Lifecycle::default();
        lifecycle.validate().unwrap();
        let saved_at = lifecycle.epoch();
        assert!(!lifecycle.pause());
        assert!(!lifecycle.save_may_enable(saved_at, |()| true));
        assert!(!lifecycle.save_may_enable(lifecycle.epoch(), |()| true));
        lifecycle = Lifecycle::default();
        assert!(
            lifecycle.save_may_enable(lifecycle.epoch(), |()| false),
            "Saving the first complete settings must enable dictation"
        );
        lifecycle = live(());
        assert!(lifecycle.pause());
        assert!(lifecycle.queue(()));
        assert_eq!(lifecycle.retired(), Some(()));
        assert!(
            lifecycle.save_may_enable(lifecycle.epoch(), |()| false),
            "A restart that applies settings is not a user pause"
        );
    }

    #[test]
    fn a_save_during_a_resume_check_supersedes_the_check() {
        let mut lifecycle = Lifecycle::<u8, ()>::default();
        let checking = lifecycle.validate().unwrap();
        assert!(lifecycle.save_may_enable(lifecycle.epoch(), |_| false));
        assert!(lifecycle.reconfigure());
        assert!(!lifecycle.queue(()));
        assert_eq!(lifecycle.start(|| Ok::<_, ()>(1)), Ok(true));
        assert!(!lifecycle.validated(checking));
        assert_eq!(lifecycle.active(), Some(&1));
    }

    #[test]
    fn owners_that_fail_to_start_still_end_the_check_they_superseded() {
        let mut lifecycle = Lifecycle::<u8, ()>::default();
        let checking = lifecycle.validate().unwrap();
        assert!(lifecycle.reconfigure());
        assert_eq!(lifecycle.start(|| Err::<u8, _>(())), Err(()));
        assert!(
            !lifecycle.validated(checking),
            "A check outlived newer settings whose owners failed to start"
        );
        assert!(matches!(lifecycle.state, State::Disabled));
    }
}
