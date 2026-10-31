//! The lane from desktop callbacks to the session owner, and the commit gate that lets Escape
//! revoke a recording's insertion while a native clipboard call is blocked.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use async_channel::Sender;

use crate::Scope;

/// Desktop observations and explicit dictation commands sent to the owner.
#[derive(Clone, Debug)]
pub enum Input {
    /// The physical shortcut became held.
    Press,
    /// The physical shortcut was released.
    Release,
    /// Space during a hold requests hands-free capture.
    Lock,
    /// S, W, or A during a hold turns the recording into a Show, which also records the screen,
    /// the window under the pointer, or an area dragged out next.
    Show(Scope),
    /// Shift tapped twice alone, which asks a Show being recorded for a numbered pin.
    Pin(Taps),
    /// Escape went down. It also reaches the focused app, so the owner decides what it cancels.
    Escape,
    /// The user asks to discard the active session, from the tray, a desktop command, or Linux's
    /// reserved cancel chord; the owner decides what it discards.
    Cancel,
    /// Another shortcut, a session change, or a macOS key-tap outage interrupted the recording.
    /// The user never asked to cancel, so the owner discards dictation without a cue and finishes
    /// a recording Show.
    Interrupt,
    /// Starts or finishes hands-free capture.
    Toggle,
    /// Starts a hands-free Show of what the scope names, as the tray's Show entries ask; the owner
    /// ignores it while a session is under way.
    StartShow(Scope),
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

/// Two quick taps of Shift alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taps {
    /// When the first tap pressed Shift.
    pub pressed: Instant,
    /// When the second tap released it.
    pub released: Instant,
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
    /// An input lane with no recording authorized.
    #[must_use]
    pub fn new(sender: Sender<Input>) -> Self {
        Self {
            sender,
            gate: Gate::default(),
        }
    }

    /// Queues `event` for the owner without waiting.
    ///
    /// Escape, cancellation, interruption, and desktop failure revoke the current recording before
    /// the owner sees them, unless it is [shielded](InsertPermit::shield). A full queue closes the
    /// lane, disabling dictation rather than losing an Escape or a release.
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
    pub(crate) fn closed(&self) -> impl Future<Output = ()> {
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

    /// Observes idle input without authorizing or revoking a recording. A later recording or
    /// closed input invalidates this observation even if that recording has already finished.
    #[must_use]
    pub fn idle(&self) -> Option<IdleInput> {
        let generation = self.gate.0.load(Ordering::Acquire);
        (!self.is_closed() && generation & (Gate::AUTHORIZED | Gate::REVOKED) != Gate::AUTHORIZED)
            .then(|| IdleInput {
                input: self.clone(),
                generation,
            })
    }

    /// Revokes the current recording without waiting for the owner, shielded or not.
    pub fn cancel(&self) {
        self.gate.revoke_current();
    }

    /// Disables native input and revokes every outstanding insertion permit, shielded or not.
    pub fn close(&self) {
        self.sender.close();
        self.cancel();
    }
}

/// An idle input generation observed before preparing an explicit clipboard copy. This does not
/// authorize insertion; it only detects recordings or shutdown that supersede the observation.
#[derive(Debug)]
pub struct IdleInput {
    input: InputSender,
    generation: u64,
}

impl IdleInput {
    /// Whether input is still idle in exactly the observed generation.
    #[must_use]
    pub fn is_current(&self) -> bool {
        !self.input.is_closed() && self.input.gate.0.load(Ordering::Acquire) == self.generation
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

    /// Leaves this recording's cancellation to whoever holds its permit: the cancels that
    /// [`InputSender::deliver`] passes on no longer revoke it on their way, while
    /// [`revoke`](Self::revoke), [`InputSender::cancel`], closing input, and a newer recording still
    /// end it. Returns whether it still held authority.
    #[must_use]
    pub fn shield(&self) -> bool {
        self.gate.shield(self.generation)
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
/// Generations count up above three state bits. A permit holds authority only while the gate equals
/// its authorized generation, shielded or not, so only one revocation of it can succeed.
#[derive(Clone, Debug, Default)]
#[expect(
    clippy::disallowed_types,
    reason = "Native Escape must revoke this recording while its owner is blocked in a clipboard OS call"
)]
struct Gate(Arc<AtomicU64>);

impl Gate {
    const AUTHORIZED: u64 = 1;
    const GENERATION_STEP: u64 = 8;
    const REVOKED: u64 = 2;
    /// [`Self::revoke_unshielded`] passes the generation by.
    const SHIELDED: u64 = 4;
    const STATE: u64 = Self::AUTHORIZED | Self::REVOKED | Self::SHIELDED;

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
        self.mark_current(generation, Self::REVOKED)
    }

    /// Shields `generation` if it is still current, reporting whether it was.
    fn shield(&self, generation: u64) -> bool {
        self.mark_current(generation, Self::SHIELDED)
    }

    /// Sets `bit` while `generation`, shielded or not, is current; reports whether it was.
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
            &|permit| permit.revoke(),
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
