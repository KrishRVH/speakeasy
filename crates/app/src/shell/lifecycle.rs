//! Service ownership across enable, pause, and quit. A stopping owner is retained
//! until acknowledgement; only its latest pending configuration may restart it.
#[derive(Default)]
pub(super) enum Lifecycle<T, C> {
    #[default]
    Disabled,
    Validating(u64),
    Running(T),
    Stopping {
        owners: T,
        pending: Option<C>,
    },
    Quitting(Option<T>),
}

impl<T, C> Lifecycle<T, C> {
    pub(super) fn validate(&mut self, epoch: u64) -> bool {
        if matches!(self, Self::Disabled | Self::Validating(_)) {
            *self = Self::Validating(epoch);
            true
        } else {
            false
        }
    }
    pub(super) fn active(&self) -> Option<&T> {
        match self {
            Self::Running(owners) => Some(owners),
            _ => None,
        }
    }
    pub(super) fn retiring(&self) -> Option<&T> {
        match self {
            Self::Stopping { owners, .. } | Self::Quitting(Some(owners)) => Some(owners),
            _ => None,
        }
    }
    pub(super) fn pausing(&self) -> bool {
        matches!(self, Self::Stopping { .. } | Self::Quitting(_))
    }
    pub(super) fn quitting(&self) -> bool {
        matches!(self, Self::Quitting(_))
    }
    pub(super) fn validating(&self, epoch: u64) -> bool {
        matches!(self, Self::Validating(current) if *current == epoch)
    }
    pub(super) fn queue(&mut self, config: C) -> bool {
        if let Self::Stopping { pending, .. } = self {
            *pending = Some(config);
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
    pub(super) fn pause(&mut self) -> bool {
        match std::mem::take(self) {
            Self::Running(owners) => {
                *self = Self::Stopping {
                    owners,
                    pending: None,
                };
                true
            },
            Self::Stopping { owners, .. } => {
                *self = Self::Stopping {
                    owners,
                    pending: None,
                };
                false
            },
            state @ Self::Quitting(_) => {
                *self = state;
                false
            },
            _ => false,
        }
    }
    pub(super) fn quit(&mut self) -> bool {
        let owners = match std::mem::take(self) {
            Self::Running(owners) | Self::Stopping { owners, .. } => Some(owners),
            state @ Self::Quitting(_) => {
                *self = state;
                return false;
            },
            _ => None,
        };
        *self = Self::Quitting(owners);
        true
    }
    pub(super) fn retired(&mut self) -> Option<C> {
        match std::mem::take(self) {
            Self::Stopping { pending, .. } => pending,
            Self::Quitting(_) => {
                *self = Self::Quitting(None);
                None
            },
            state => {
                *self = state;
                None
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Lifecycle;
    #[test]
    fn validating_never_discards_live_or_retiring_owners() {
        use std::sync::mpsc;

        struct Owner(mpsc::Sender<()>);
        impl Drop for Owner {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
            }
        }
        let (completion, dropped) = mpsc::channel();
        let mut state = Lifecycle::<_, ()>::Running(Owner(completion));
        assert!(!state.validate(1));
        assert!(dropped.try_recv().is_err());
        assert!(state.pause());
        assert!(!state.validate(2));
        assert!(dropped.try_recv().is_err());
        state.retired();
        dropped.try_recv().unwrap();
        assert!(state.validate(3));
        assert!(state.validating(3));
        assert!(state.quit());
        assert!(!state.validate(4));
    }
    #[test]
    fn pause_invalidates_validation_and_repeated_pause_discards_resume() {
        let mut state = Lifecycle::<(), u8>::Validating(1);
        assert!(!state.pause());
        assert!(!state.validating(1));
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
