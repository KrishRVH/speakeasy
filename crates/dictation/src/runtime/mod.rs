//! Thread ownership and presentation interface for the dictation owner. Stop revokes authority
//! immediately; `stopped` acknowledges owned native cleanup.

mod microphone;
mod owner;
mod session;
mod timeline;
mod worker;

use std::{
    convert::Infallible,
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
#[cfg(target_os = "macos")]
use speakeasy_platform::InputMonitor;
use speakeasy_platform::{Input, InputSender, OwnedThread};
use tokio::{sync::watch, task::JoinError};

pub(crate) use self::session::SessionId;
#[cfg(target_os = "macos")]
use crate::ports::Desktop;
use crate::{config::Config, ports::Ports};

/// The status while a speech worker loads, shared by the owner, Settings, and the tray.
pub const LOADING: &str = "Loading local model…";

/// Where the latest session is, from microphone startup to its outcome.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// No session has run since dictation started.
    #[default]
    Idle,
    /// The microphone is opening.
    Starting,
    /// Audio is arriving.
    Recording,
    /// Capture is finishing its last audio.
    Stopping,
    /// Speech is being recognized or inserted.
    Processing,
    /// Text was submitted to the focused application.
    Done,
    /// The recording held no speech.
    Empty,
    /// The user discarded the session.
    Cancelled,
    /// The session failed; the snapshot message explains why.
    Error,
}

impl Phase {
    /// Whether a session is in progress rather than finished.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Recording | Self::Stopping | Self::Processing
        )
    }
}

/// Whether the local speech model can serve a recording.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModelState {
    /// The model is starting or recovering.
    Loading,
    /// The model is warm.
    Ready,
    /// No model is running.
    #[default]
    Unavailable,
}

/// The presentation of owned session state that Settings, the tray, and the pill render.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The shell lifecycle it belongs to; an owner publishes only while its epoch is current.
    pub epoch: u64,
    /// The latest session, so a new session differs even when its phase repeats.
    pub id: u64,
    /// The latest session's phase.
    pub phase: Phase,
    /// Whether capture continues until the next press.
    pub hands_free: bool,
    /// The latest speech meter level in `0.0..=1.0`.
    pub level: f32,
    /// Advances with every level report, so a repeated level still reaches the meter.
    pub meter_tick: u64,
    /// When the latest session started; the pill's recording clock counts from it.
    pub started: Instant,
    /// The status or failure text Settings shows.
    pub message: String,
    /// The speech model's readiness.
    pub model: ModelState,
    /// Whether native input can start capture; false while desktop startup is pending.
    pub desktop_ready: bool,
    /// The dictation shortcut as people see it.
    pub shortcut: Arc<str>,
    /// The cancellation shortcut as people see it.
    pub cancel_shortcut: Arc<str>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            epoch: 0,
            id: 0,
            phase: Phase::Idle,
            hands_free: false,
            level: 0.0,
            meter_tick: 0,
            started: now(),
            message: String::new(),
            model: ModelState::Unavailable,
            desktop_ready: true,
            shortcut: speakeasy_platform::SHORTCUT.into(),
            cancel_shortcut: speakeasy_platform::CANCEL_SHORTCUT.into(),
        }
    }
}

/// A capture thread's report about the session that opened it.
pub(crate) enum CaptureEvent {
    /// Samples arrived; the capture thread took the duration to open and start the device.
    Ready(SessionId, Duration),
    Level(SessionId, f32),
    Finished(SessionId, anyhow::Result<Option<Vec<u8>>>),
}

impl CaptureEvent {
    const fn session(&self) -> SessionId {
        match self {
            Self::Ready(session, _) | Self::Level(session, _) | Self::Finished(session, _) => {
                *session
            },
        }
    }
}

/// Controls for the dictation owner. `Owner` hosts its loop: a dedicated thread, joined on drop
/// once stop has been requested, or a task on the test executor for paused-clock tests.
pub struct Runtime<Owner = OwnedThread> {
    owner: Owner,
    /// The lane native input and shell commands use to reach the owner.
    pub input: InputSender,
    configuration: watch::Sender<Config>,
    stop: Sender<Infallible>,
}

impl Runtime {
    /// Starts the owner on its own thread with native capture, recognition, and insertion,
    /// returning the shortcut monitor that feeds it.
    ///
    /// # Errors
    /// Returns an error if native input or the owner thread cannot start.
    #[cfg(target_os = "macos")]
    pub fn start(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
    ) -> anyhow::Result<(Self, InputMonitor)> {
        let (input, inputs) = input_lane();
        let (monitor, inserter) = speakeasy_platform::prepare(input.clone())?;
        let ports = Desktop {
            inserter,
            timing: std::env::var_os("SPEAKEASY_TIMING").is_some(),
        };
        let runtime = Self::host(
            config,
            snapshots,
            ports,
            input,
            inputs,
            Wiring::run_on_own_thread,
        )?;
        Ok((runtime, monitor))
    }

    #[cfg(test)]
    fn start_with<P: Ports>(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
        ports: P,
    ) -> anyhow::Result<Self> {
        let (input, inputs) = input_lane();
        Self::host(
            config,
            snapshots,
            ports,
            input,
            inputs,
            Wiring::run_on_own_thread,
        )
    }

    /// Whether the owner still accepts input.
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.input.is_closed() && !self.owner.is_finished()
    }

    /// Resolves once the owner has finished its native cleanup and exited.
    pub fn stopped(&self) -> impl Future<Output = ()> + use<> {
        self.owner.exited()
    }
}

impl<Owner> Runtime<Owner> {
    /// Wires the owner's lanes and hands its loop to `run`, which decides where it executes.
    fn host<P: Ports>(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
        ports: P,
        input: InputSender,
        inputs: Receiver<Input>,
        run: impl FnOnce(Wiring<P>) -> io::Result<Owner>,
    ) -> anyhow::Result<Self> {
        let epoch = snapshots.borrow().epoch;
        let (configuration, changes) = watch::channel(config);
        let (stop, stopping) = async_channel::bounded(1);
        let owner = run(Wiring {
            changes,
            inputs,
            snapshots,
            permits: input.clone(),
            stopping,
            ports,
            epoch,
        })?;
        Ok(Self {
            owner,
            input,
            configuration,
            stop,
        })
    }

    /// Applies new settings, revoking any pending insertion first.
    pub fn configure(&self, config: Config) {
        // Revoke now, so a preparing insertion cannot commit before the owner applies the change.
        self.input.cancel();
        self.configuration.send_replace(config);
    }

    /// Wakes the owner on its stop lane before closing input, so a normal pause never reports a
    /// shortcut failure or drains queued presses.
    pub fn request_stop(&self) {
        self.stop.close();
        self.input.close();
    }
}

impl<Owner> Drop for Runtime<Owner> {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// The owner's ends of its lanes, with its ports and the lifecycle epoch it publishes under.
struct Wiring<P> {
    changes: watch::Receiver<Config>,
    inputs: Receiver<Input>,
    snapshots: watch::Sender<Snapshot>,
    permits: InputSender,
    stopping: Receiver<Infallible>,
    ports: P,
    epoch: u64,
}

impl<P: Ports> Wiring<P> {
    fn run_on_own_thread(self) -> io::Result<OwnedThread> {
        OwnedThread::spawn("dictation", move || self.run_on_current_thread())
    }

    fn run_on_current_thread(self) {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(owner::run(self)),
            Err(error) => self.fail_startup(&error),
        }
    }

    /// Reports the failure unless a newer shell lifecycle owns the snapshot.
    fn fail_startup(&self, error: &io::Error) {
        self.snapshots.send_if_modified(|snapshot| {
            if snapshot.epoch != self.epoch {
                return false;
            }
            snapshot.phase = Phase::Error;
            snapshot.message = format!("Could not start dictation worker: {error}");
            true
        });
    }
}

/// Follows tokio's paused clock in tests, so gesture deadlines and snapshots share one timeline.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

fn input_lane() -> (InputSender, Receiver<Input>) {
    let (sender, inputs) = async_channel::bounded(64);
    (InputSender::new(sender), inputs)
}

/// Awaits a task that no longer holds session authority, so its cleanup completes even when its
/// result can no longer matter.
async fn reap<T>(task: impl Future<Output = Result<T, JoinError>>) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A reaped task's result or panic belongs to no current session; awaiting it still completes its owned cleanup"
    )]
    let _ = task.await;
}

#[cfg(test)]
mod fixture;
#[cfg(test)]
mod tests;
