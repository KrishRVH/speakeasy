//! The lane from desktop callbacks to the session owner, and the commit gate that lets Escape
//! revoke a recording's insertion while a native clipboard call is blocked.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use async_channel::Sender;

/// Desktop observations and explicit dictation commands sent to the owner.
#[derive(Debug, Clone)]
pub enum Input {
    /// The physical shortcut became held.
    Press,
    /// The physical shortcut was released.
    Release,
    /// Space during a hold requests hands-free capture.
    Lock,
    /// S during a hold turns the recording into a Show, which also captures the screen.
    Show,
    /// Discards the active session and revokes pending insertion.
    Cancel,
    /// Starts or finishes hands-free capture.
    Toggle,
    /// Desktop access failed; the message explains how to resume.
    Unavailable(String),
    /// Shortcut and insertion preparation has completed.
    DesktopReady {
        /// The accepted dictation shortcut for display in Settings.
        shortcut: String,
        /// The accepted cancellation shortcut for display in Settings.
        cancel: String,
    },
}

/// The lane from OS callbacks to the session owner, carrying the recording's commit gate.
///
/// Clipboard preparation may block; Escape must still be able to invalidate the pending paste. A
/// permit's successful commit is the point of no return.
#[derive(Clone, Debug)]
pub struct InputSender {
    sender: Sender<Input>,
    gate: Gate,
}

impl InputSender {
    /// Creates an input lane with no recording authorized.
    #[must_use]
    pub fn new(sender: Sender<Input>) -> Self {
        Self {
            sender,
            gate: Gate::default(),
        }
    }

    /// Queues `event` for the owner without waiting.
    ///
    /// Cancellation and desktop failure revoke the current recording before the owner sees them. A
    /// full queue closes the lane, disabling dictation rather than losing an Escape or a release.
    pub fn deliver(&self, event: Input) {
        if matches!(event, Input::Cancel | Input::Unavailable(_)) {
            self.cancel();
        }
        if self.sender.try_send(event).is_err() {
            self.close();
        }
    }

    /// Reports that desktop access failed, then disables native input and insertion.
    pub fn fail(&self, message: String) {
        self.deliver(Input::Unavailable(message));
        self.close();
    }

    /// Whether native input and insertion are permanently disabled.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// Resolves once native input and insertion are permanently disabled.
    #[cfg(target_os = "linux")]
    pub(crate) fn closed(&self) -> impl Future<Output = ()> + '_ {
        self.sender.closed()
    }

    /// Authorizes a new recording, invalidating every older permit.
    ///
    /// Returns `None` once the lane is closed or generations run out.
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
        // A close racing this call may have revoked the previous generation instead of this one.
        if self.is_closed() {
            permit.revoke();
            return None;
        }
        Some(permit)
    }

    /// Whether the current recording still has insertion authority.
    #[must_use]
    pub fn active(&self) -> bool {
        self.gate.is_authorized()
    }

    /// Revokes the current recording without waiting for the owner.
    pub fn cancel(&self) {
        self.gate.revoke_current();
    }

    /// Disables native input and revokes every outstanding insertion permit.
    pub fn close(&self) {
        self.sender.close();
        self.cancel();
    }
}

/// Authorization for exactly one recording. Starting another recording never authorizes an old
/// clipboard operation that is still preparing its input.
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

    /// Revokes only this recording, including while an OS operation is blocked. An obsolete permit
    /// cannot revoke a newer recording.
    pub fn revoke(&self) {
        self.gate.revoke(self.generation);
    }

    /// Revokes only this recording, reporting whether this call ended its authority: false when an
    /// earlier revocation such as Escape, a commit, or a newer recording ended it.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub(crate) fn forfeit(&self) -> bool {
        self.gate.revoke(self.generation)
    }

    /// Whether this recording still has insertion authority.
    #[must_use]
    pub fn active(&self) -> bool {
        self.gate.is_current(self.generation)
    }

    /// Consumes this recording's authority immediately before native input is submitted, returning
    /// whether it still held authority.
    #[must_use]
    pub fn commit(&self) -> bool {
        self.gate.revoke(self.generation)
    }
}

/// The current recording generation, shared with the callbacks that revoke it.
///
/// Generations count up above two state bits. A permit holds authority only while the gate equals
/// its authorized generation, so only one revocation of it can succeed.
#[derive(Clone, Debug, Default)]
#[expect(
    clippy::disallowed_types,
    reason = "Native Escape must revoke this recording while its owner is blocked in a clipboard OS call"
)]
struct Gate(Arc<AtomicU64>);

impl Gate {
    const AUTHORIZED: u64 = 1;
    const GENERATION_STEP: u64 = 4;
    const REVOKED: u64 = 2;
    const STATE: u64 = Self::AUTHORIZED | Self::REVOKED;

    /// Authorizes the next generation, superseding the current one; none once generations run out.
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

    /// Revokes `generation` if it is still current, reporting whether it was.
    fn revoke(&self, generation: u64) -> bool {
        self.0
            .compare_exchange(
                generation,
                generation | Self::REVOKED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn revoke_current(&self) {
        self.0.fetch_or(Self::REVOKED, Ordering::AcqRel);
    }

    fn is_current(&self, generation: u64) -> bool {
        self.0.load(Ordering::Acquire) == generation
    }

    fn is_authorized(&self) -> bool {
        self.0.load(Ordering::Acquire) & Self::STATE == Self::AUTHORIZED
    }

    #[cfg(target_os = "linux")]
    fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    #[test]
    fn closed_input_cannot_grant_new_insertion_authority() -> anyhow::Result<()> {
        let (sender, _events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Missing initial permit")?;
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
        let permit = input.begin().context("First insertion permit")?;
        assert!(input.active());
        input.deliver(Input::Cancel);
        assert!(
            !permit.commit(),
            "Escape must revoke without the owner draining its queue"
        );
        let next = input.begin().context("New insertion permit")?;
        assert!(!permit.commit(), "New recording authorized an old paste");
        assert!(next.commit());
        input.deliver(Input::Cancel);
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
    fn full_input_queue_cancels_instead_of_losing_escape() -> anyhow::Result<()> {
        let (sender, events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Insertion permit")?;
        assert!(permit.active());
        input.deliver(Input::Press);
        input.deliver(Input::Cancel);
        assert!(events.is_closed());
        assert!(!permit.commit());
        Ok(())
    }
}
