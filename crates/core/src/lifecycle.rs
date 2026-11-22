use std::mem;

#[derive(Debug)]
pub struct Lifecycle<Owners, Restart> {
    state: State<Owners, Restart>,
    held: bool,
}

#[derive(Debug, Default)]
enum State<Owners, Restart> {
    #[default]
    Disabled,
    Validating,
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
            held: false,
        }
    }
}

impl<Owners, Restart> Lifecycle<Owners, Restart> {
    pub fn is_running(&self, live: impl FnOnce(&Owners) -> bool) -> bool {
        match &self.state {
            State::Running(owners) => live(owners),
            State::Validating => true,
            State::Disabled | State::Stopping { .. } | State::Quitting(_) => false,
        }
    }

    pub fn save_may_enable(&self, live: impl FnOnce(&Owners) -> bool) -> bool {
        !self.held || self.is_running(live) || self.has_pending()
    }

    pub fn validate(&mut self) -> bool {
        if !self.may_start() {
            return false;
        }
        self.state = State::Validating;
        self.held = false;
        true
    }

    pub fn validated(&mut self) -> bool {
        if !matches!(self.state, State::Validating) {
            return false;
        }
        self.state = State::Disabled;
        true
    }

    pub fn start<E>(&mut self, launch: impl FnOnce() -> Result<Owners, E>) -> Result<bool, E> {
        if !self.may_start() {
            return Ok(false);
        }
        self.state = State::Disabled;
        let owners = launch()?;
        self.state = State::Running(owners);
        Ok(true)
    }

    pub const fn active(&self) -> Option<&Owners> {
        match &self.state {
            State::Running(owners) => Some(owners),
            State::Disabled | State::Validating | State::Stopping { .. } | State::Quitting(_) => {
                None
            },
        }
    }

    pub const fn active_mut(&mut self) -> Option<&mut Owners> {
        match &mut self.state {
            State::Running(owners) => Some(owners),
            State::Disabled | State::Validating | State::Stopping { .. } | State::Quitting(_) => {
                None
            },
        }
    }

    pub const fn retiring(&self) -> Option<&Owners> {
        match &self.state {
            State::Stopping { owners, .. } | State::Quitting(Some(owners)) => Some(owners),
            State::Disabled | State::Validating | State::Running(_) | State::Quitting(None) => None,
        }
    }

    pub const fn is_pausing(&self) -> bool {
        matches!(self.state, State::Stopping { .. } | State::Quitting(_))
    }

    pub const fn is_quitting(&self) -> bool {
        matches!(self.state, State::Quitting(_))
    }

    pub fn queue(&mut self, restart: Restart) -> bool {
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
        matches!(self.state, State::Disabled | State::Validating)
    }

    pub fn pause(&mut self) -> bool {
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
            State::Disabled | State::Validating => (State::Disabled, false),
            quitting @ State::Quitting(_) => {
                self.state = quitting;
                return false;
            },
        };
        self.state = state;
        self.held = true;
        started
    }

    pub const fn forget_pause(&mut self) {
        self.held = false;
    }

    pub fn quit(&mut self) -> bool {
        let owners = match mem::take(&mut self.state) {
            State::Running(owners) | State::Stopping { owners, .. } => Some(owners),
            quitting @ State::Quitting(_) => {
                self.state = quitting;
                return false;
            },
            State::Disabled | State::Validating => None,
        };
        self.state = State::Quitting(owners);
        true
    }

    pub fn retired(&mut self) -> Option<Restart> {
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
            State::Disabled | State::Validating | State::Running(_) => None,
        }
    }
}

#[cfg(test)]
mod tests;
