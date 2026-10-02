//! Native macOS input, presentation, insertion, speech-engine, and child-process ownership.
//!
//! Callbacks deliver events to the session owner. Insertion permits authorize one recording and can
//! be revoked while a native clipboard call is blocked. The portable policy and ownership types
//! also build on other Unix hosts, so the service's tests run without a Mac.

mod insertion;
#[cfg(any(target_os = "macos", test))]
mod keyboard;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod monitor;
mod owned_thread;
mod process;
mod scheduling;
pub mod speech;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use async_channel::Sender;

#[cfg(target_os = "macos")]
pub use self::macos::{
    InputMonitor, configure_pill, reduced_motion, set_pill_visible, set_settings_visible,
    show_error,
};
pub use self::{
    insertion::Delivery,
    owned_thread::OwnedThread,
    process::{ProcessGroup, exit_now, private_stdout},
    scheduling::{Responsive, prefer_responsive_thread},
};

/// The pill window's width in device-independent pixels.
pub const PILL_WIDTH: u16 = 400;
/// The pill window's height in device-independent pixels.
pub const PILL_HEIGHT: u16 = 100;

/// The hold-to-talk shortcut as people see it.
pub const SHORTCUT: &str = "Fn";

/// The shortcut that discards a recording, as people see it.
pub const CANCEL_SHORTCUT: &str = "Escape";

/// Whether dictation runs, as the tray and Settings present it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    /// Dictation resources are live.
    Running,
    /// Dictation resources are being released.
    Pausing,
    /// Dictation is off.
    Paused,
}

/// What dictation is doing, as the tray presents it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// No recording is in progress.
    Idle,
    /// Capture is starting or recording.
    Dictating,
    /// Capture is ending or speech is being transcribed.
    Finishing,
}

/// Desktop observations and explicit dictation commands sent to the owner.
#[derive(Debug, Clone)]
pub enum Input {
    /// The physical shortcut became held.
    Press,
    /// The physical shortcut was released.
    Release,
    /// Space during a hold requests hands-free capture.
    Lock,
    /// Discards the active session and revokes pending insertion.
    Cancel,
    /// Starts or finishes hands-free capture.
    Toggle,
    /// Desktop access failed; the message explains how to resume.
    Unavailable(String),
    /// Shortcut and insertion preparation has completed.
    DesktopReady,
}

/// The lane from OS callbacks to the session owner, carrying the recording's commit gate.
///
/// Clipboard preparation may block; Escape must still be able to invalidate the pending paste. A
/// permit's successful commit is the point of no return.
#[derive(Clone)]
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

    /// Whether native input and insertion are permanently disabled.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
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
#[derive(Clone)]
pub struct InsertPermit {
    gate: Gate,
    generation: u64,
}

impl InsertPermit {
    /// Revokes only this recording, including while an OS operation is blocked. An obsolete permit
    /// cannot revoke a newer recording.
    pub fn revoke(&self) {
        self.gate.revoke(self.generation);
    }

    /// Whether this recording still has insertion authority.
    #[must_use]
    pub fn active(&self) -> bool {
        self.gate.is_current(self.generation)
    }

    /// Consumes this recording’s authority immediately before native input is submitted, returning
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
#[derive(Clone, Default)]
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
}

/// Submission outcome; native input cannot prove that the target accepted text.
#[derive(Debug, PartialEq, Eq)]
pub enum Inserted {
    /// Native input was submitted once.
    Sent,
    /// Text remains on the clipboard with a manual-paste remedy.
    Copied(&'static str),
    /// Insertion authority was revoked before submission.
    Cancelled,
    /// Insertion is unavailable or incomplete; the message explains how to recover.
    Unavailable(&'static str),
}

/// Serializes native clipboard and input operations without blocking the session owner.
///
/// Requests wait for their turn without occupying a blocking thread, and each keeps its turn until
/// its native work ends, even if the caller stops waiting, so a cancelled request never races a
/// newer clipboard write.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub struct Inserter {
    #[expect(
        clippy::disallowed_types,
        reason = "Serialize irreversible clipboard preparation on blocking native workers; cancellation remains on the independent generation gate"
    )]
    serial: Arc<tokio::sync::Mutex<()>>,
}

#[cfg(target_os = "macos")]
impl Inserter {
    /// Submits authorized text through the chosen delivery.
    ///
    /// # Errors
    /// Returns native input failures or a failed blocking worker.
    pub async fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        delivery: Delivery,
    ) -> anyhow::Result<Inserted> {
        let turn = Arc::clone(&self.serial).lock_owned().await;
        if !permit.active() {
            return Ok(Inserted::Cancelled);
        }
        tokio::task::spawn_blocking(move || {
            insertion::wait_for_released_modifiers(&permit, || Ok(macos::modifiers_down()))?;
            let inserted = macos::insert(&text, &permit, delivery);
            // Moving the turn in holds it through native work even if the caller stops awaiting.
            drop(turn);
            inserted
        })
        .await?
    }
}

/// Starts desktop observation and creates its matching insertion adapter.
///
/// # Errors
/// Returns thread startup or native desktop initialization failures.
#[cfg(target_os = "macos")]
pub fn prepare(input: InputSender) -> anyhow::Result<(InputMonitor, Inserter)> {
    Ok((
        InputMonitor::start(input)?,
        Inserter {
            serial: Arc::default(),
        },
    ))
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
