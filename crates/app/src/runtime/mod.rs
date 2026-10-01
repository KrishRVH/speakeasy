//! Thread ownership and presentation interface for the dictation owner. Stop revokes authority
//! immediately; `stopped` acknowledges owned native cleanup.

mod microphone;
mod owner;
mod session;
mod worker;

use std::{convert::Infallible, io, sync::Arc, time::Instant};

use async_channel::{Receiver, Sender};
use speakeasy_platform::{Input, InputMonitor, InputSender, OwnedThread};
use tokio::{sync::watch, task::JoinError};

pub(crate) use self::session::SessionId;
use crate::{
    config::Config,
    ports::{Desktop, Ports},
};

/// The status while a speech worker loads, shared by the owner, Settings, and the tray.
pub(crate) const LOADING: &str = "Loading local model…";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Idle,
    Starting,
    Recording,
    Stopping,
    Processing,
    Done,
    Empty,
    Cancelled,
    Error,
}

impl Phase {
    pub(crate) const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Recording | Self::Stopping | Self::Processing
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ModelState {
    Loading,
    Ready,
    #[default]
    Unavailable,
}

/// The presentation of owned session state that Settings, the tray, and the pill render.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    /// The shell lifecycle it belongs to; an owner publishes only while its epoch is current.
    pub epoch: u64,
    /// The latest session, so a new session differs even when its phase repeats.
    pub id: u64,
    pub phase: Phase,
    pub hands_free: bool,
    pub level: f32,
    /// Advances with every level report, so a repeated level still reaches the meter.
    pub meter_tick: u64,
    /// When the latest session started; the pill's recording clock counts from it.
    pub started: Instant,
    pub message: String,
    pub model: ModelState,
    /// Whether native input can start capture; false while desktop startup is pending.
    pub desktop_ready: bool,
    pub shortcut: Arc<str>,
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
    Ready(SessionId),
    Level(SessionId, f32),
    Finished(SessionId, anyhow::Result<Option<Vec<u8>>>),
}

impl CaptureEvent {
    const fn session(&self) -> SessionId {
        match self {
            Self::Ready(session) | Self::Level(session, _) | Self::Finished(session, _) => *session,
        }
    }
}

/// Controls for the dictation owner. `Owner` hosts its loop: a dedicated thread, joined on drop
/// once stop has been requested, or a task on the test executor for paused-clock tests.
pub(crate) struct Runtime<Owner = OwnedThread> {
    owner: Owner,
    pub input: InputSender,
    configuration: watch::Sender<Config>,
    stop: Sender<Infallible>,
}

impl Runtime {
    pub(crate) fn start(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
    ) -> anyhow::Result<(Self, InputMonitor)> {
        let (input, inputs) = input_lane();
        let (monitor, inserter) =
            speakeasy_platform::prepare(input.clone(), config.desktop_options())?;
        let ports = Desktop { inserter };
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

    pub(crate) fn is_running(&self) -> bool {
        !self.input.is_closed() && !self.owner.is_finished()
    }

    /// Resolves once the owner has finished its native cleanup and exited.
    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + use<> {
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

    pub(crate) fn configure(&self, config: Config) {
        // Revoke now, so a preparing insertion cannot commit before the owner applies the change.
        self.input.cancel();
        self.configuration.send_replace(config);
    }

    /// Wakes the owner on its stop lane before closing input, so a normal pause never reports a
    /// shortcut failure or drains queued presses.
    pub(crate) fn request_stop(&self) {
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
