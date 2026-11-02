use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use async_channel::Sender;

use crate::Scope;

#[derive(Clone, Debug)]
pub enum Input {
    Press,
    Release,
    Lock,
    Show(Scope),
    Pin(Taps),
    Escape,
    Cancel,
    Interrupt,
    Toggle,
    PasteLast,
    StartShow(Scope),
    Unavailable(String),
    DesktopReady { shortcut: String, cancel: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taps {
    pub pressed: Instant,
    pub released: Instant,
}

#[derive(Clone, Debug)]
pub struct InputSender {
    sender: Sender<Input>,
    gate: Gate,
}

impl InputSender {
    #[must_use]
    pub fn new(sender: Sender<Input>) -> Self {
        Self {
            sender,
            gate: Gate::default(),
        }
    }

    pub fn deliver(&self, event: Input) {
        if matches!(
            event,
            Input::Escape | Input::Cancel | Input::Interrupt | Input::Unavailable(_)
        ) {
            self.gate.revoke_unshielded();
        }
        if self.sender.try_send(event).is_err() {
            self.close();
        }
    }

    pub fn fail(&self, message: String) {
        self.deliver(Input::Unavailable(message));
        self.close();
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn closed(&self) -> impl Future<Output = ()> {
        self.sender.closed()
    }

    #[must_use]
    pub fn begin(&self) -> Option<InsertPermit> {
        if self.is_closed() {
            return None;
        }
        let generation = self.gate.advance()?;
        let permit = InsertPermit {
            gate: self.gate.clone(),
            generation,
        };

        if self.is_closed() {
            permit.revoke();
            return None;
        }
        Some(permit)
    }

    #[must_use]
    pub fn active(&self) -> bool {
        self.gate.is_authorized()
    }

    #[must_use]
    pub fn idle(&self) -> Option<IdleInput> {
        let generation = self.gate.idle_generation()?;
        (!self.is_closed()).then(|| IdleInput {
            input: self.clone(),
            generation,
        })
    }

    pub fn cancel(&self) {
        self.gate.revoke_current();
    }

    pub fn close(&self) {
        self.sender.close();
        self.cancel();
    }
}

#[derive(Debug)]
pub struct IdleInput {
    input: InputSender,
    generation: u64,
}

impl IdleInput {
    #[must_use]
    pub fn is_current(&self) -> bool {
        !self.input.is_closed() && self.input.gate.is_exactly(self.generation)
    }
}

#[derive(Clone, Debug)]
pub struct InsertPermit {
    gate: Gate,
    generation: u64,
}

impl InsertPermit {
    #[cfg(target_os = "linux")]
    pub(crate) fn same_recording(&self, other: &Self) -> bool {
        self.gate.is_same(&other.gate) && self.generation == other.generation
    }

    #[expect(
        clippy::must_use_candidate,
        reason = "Ending a recording needs no answer; only a lost clipboard selection asks whether it ended the current one"
    )]
    pub fn revoke(&self) -> bool {
        self.gate.revoke(self.generation)
    }

    #[must_use]
    pub fn shield(&self) -> bool {
        self.gate.shield(self.generation)
    }

    #[must_use]
    pub fn active(&self) -> bool {
        self.gate.is_current(self.generation)
    }

    #[must_use]
    pub fn commit(&self) -> bool {
        self.gate.revoke(self.generation)
    }
}

#[derive(Clone, Debug, Default)]
#[expect(
    clippy::disallowed_types,
    reason = "Native Escape must revoke this recording while its owner is blocked in a clipboard OS call"
)]
struct Gate(Arc<AtomicU64>);

const _: () = assert!(Gate::STATE == (Gate::AUTHORIZED | Gate::REVOKED | Gate::SHIELDED));

impl Gate {
    const AUTHORIZED: u64 = 1;
    const GENERATION_STEP: u64 = 1 << Self::STATE_BITS;
    const REVOKED: u64 = 2;
    const SHIELDED: u64 = 4;
    const STATE: u64 = Self::GENERATION_STEP - 1;
    const STATE_BITS: u32 = 3;

    fn advance(&self) -> Option<u64> {
        let next = |current: u64| {
            Some((current & !Self::STATE).checked_add(Self::GENERATION_STEP)? | Self::AUTHORIZED)
        };
        let previous = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, next)
            .ok()?;
        next(previous)
    }

    fn revoke(&self, generation: u64) -> bool {
        self.mark_current(generation, Self::REVOKED)
    }

    fn shield(&self, generation: u64) -> bool {
        self.mark_current(generation, Self::SHIELDED)
    }

    fn mark_current(&self, generation: u64, bit: u64) -> bool {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current & !Self::SHIELDED == generation).then_some(current | bit)
            })
            .is_ok()
    }

    fn revoke_current(&self) {
        self.0.fetch_or(Self::REVOKED, Ordering::AcqRel);
    }

    #[expect(
        clippy::let_underscore_must_use,
        reason = "A shielded generation is left to its permit, so a refusal needs no handling"
    )]
    fn revoke_unshielded(&self) {
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current & Self::SHIELDED == 0).then_some(current | Self::REVOKED)
            });
    }

    fn idle_generation(&self) -> Option<u64> {
        let generation = self.0.load(Ordering::Acquire);
        (generation & (Self::AUTHORIZED | Self::REVOKED) != Self::AUTHORIZED).then_some(generation)
    }

    fn is_exactly(&self, generation: u64) -> bool {
        self.0.load(Ordering::Acquire) == generation
    }

    fn is_current(&self, generation: u64) -> bool {
        self.0.load(Ordering::Acquire) & !Self::SHIELDED == generation
    }

    fn is_authorized(&self) -> bool {
        self.0.load(Ordering::Acquire) & (Self::AUTHORIZED | Self::REVOKED) == Self::AUTHORIZED
    }

    #[cfg(target_os = "linux")]
    fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context as _;

    use super::*;

    #[test]
    fn closed_input_cannot_grant_new_insertion_authority() -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Recording")?;
        input.close();
        assert!(!permit.active());
        assert!(input.begin().is_none());
        assert!(!input.active());
        Ok(())
    }

    #[test]
    fn escape_during_clipboard_preparation_prevents_commit() -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(4);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Previous recording")?;
        assert!(input.active());
        input.deliver(Input::Escape);
        assert!(
            !permit.commit(),
            "Escape must revoke without the owner draining its queue"
        );
        let next = input.begin().context("Current recording")?;
        assert!(!permit.commit(), "New recording authorized an old paste");
        assert!(next.commit());
        input.deliver(Input::Escape);
        assert!(!next.commit(), "The same result was submitted twice");
        Ok(())
    }

    #[test]
    fn obsolete_permit_cannot_revoke_a_newer_recording() -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(4);
        let input = InputSender::new(sender);
        let obsolete = input.begin().context("Obsolete recording")?;
        let current = input.begin().context("Current recording")?;
        obsolete.revoke();
        assert!(current.active());
        assert!(current.commit());
        Ok(())
    }

    #[test]
    fn delivered_cancels_pass_a_shielded_recording_by() -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(8);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Recording")?;
        assert!(permit.shield());
        for event in [
            Input::Escape,
            Input::Cancel,
            Input::Interrupt,
            Input::Unavailable(String::new()),
        ] {
            input.deliver(event);
        }
        assert!(
            permit.active(),
            "A delivered cancel revoked a shielded recording"
        );
        assert!(permit.commit());
        assert!(!permit.commit(), "A shielded recording committed twice");
        Ok(())
    }

    #[test]
    fn a_shielded_recording_still_ends_by_its_permit_a_newer_one_cancel_or_close()
    -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let ends: [&dyn Fn(&InsertPermit); 4] = [
            &|permit| {
                permit.revoke();
            },
            &|_| assert!(input.begin().is_some()),
            &|_| input.cancel(),
            &|_| input.close(),
        ];
        for end in ends {
            let permit = input.begin().context("Recording")?;
            assert!(permit.shield());
            end(&permit);
            assert!(!permit.active());
            assert!(!permit.commit());
        }
        Ok(())
    }

    #[test]
    fn a_recording_a_delivered_cancel_revoked_cannot_be_shielded() -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Recording")?;
        input.deliver(Input::Escape);
        assert!(!permit.shield());
        assert!(!permit.active());
        Ok(())
    }

    #[test]
    fn full_input_queue_cancels_instead_of_losing_escape() -> anyhow::Result<()> {
        let (sender, events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Recording")?;
        assert!(permit.active());
        input.deliver(Input::Press);
        input.deliver(Input::Escape);
        assert!(events.is_closed());
        assert!(!permit.commit());
        Ok(())
    }
}
