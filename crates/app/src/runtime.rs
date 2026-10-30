//! Thread ownership, production ports, and presentation interface for the dictation owner. Stop
//! revokes authority immediately; `stopped` acknowledges owned native cleanup.

mod device;
mod owner;
pub(crate) mod ports;
mod session;
mod worker;

use std::{convert::Infallible, io, sync::Arc, time::Instant};

use async_channel::{Receiver, Sender, TrySendError};
use speakeasy_platform::{
    Delivery, Input, InputMonitor, InputSender, InsertPermit, Inserted, Inserter, OwnedThread,
    Scope,
};
use speakeasy_show::{Ending, Narration};
use tokio::{sync::watch, task::JoinError};

use self::ports::{CaptureEvent, Footage, Ports, Published, SessionId, ShowEvent};
use crate::{
    audio::Capture,
    config::{Change, Config, ValidConfig},
    local_speech::LocalSpeech,
    show::{self, Canvas, ScreenRecorder},
    sounds::{Cue, Sounds},
};

/// The status while a speech worker loads, shared by the owner, Settings, and the tray.
pub(crate) const LOADING: &str = "Loading local model…";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Idle,
    Starting,
    Recording,
    Stopping,
    Processing,
    Done,
    /// Text or a Show summary is on the clipboard for the user to paste; `message` says how.
    Copied,
    Empty,
    Cancelled,
    /// A Show was cancelled because its scope found nothing to record; `message` says what.
    Unchosen,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelState {
    Loading,
    Ready,
    Unavailable,
}

/// Which of the shell's own announcements a snapshot follows. Each announcement advances it, and an
/// owner presents only while the epoch it started under is current.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SnapshotEpoch(u64);

impl SnapshotEpoch {
    pub(crate) const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// The presentation of owned session state that Settings, the tray, and the pill render.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub epoch: SnapshotEpoch,
    /// The latest session, so a new session differs even when its phase repeats.
    pub session: u64,
    pub phase: Phase,
    pub hands_free: bool,
    /// Whether the latest session became a Show, recording the screen as well as the microphone.
    pub show: bool,
    /// How the latest Show's screen recording ended once the Show is copied, as its message says;
    /// `Finished` before then and for dictation.
    pub ending: Ending,
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
            epoch: SnapshotEpoch::default(),
            session: 0,
            phase: Phase::Idle,
            hands_free: false,
            show: false,
            ending: Ending::Finished,
            started: now(),
            message: String::new(),
            model: ModelState::Unavailable,
            desktop_ready: true,
            shortcut: speakeasy_platform::SHORTCUT.into(),
            cancel_shortcut: speakeasy_platform::CANCEL_SHORTCUT.into(),
        }
    }
}

/// The latest microphone level, on its own lane so that level reports never re-present session
/// state. Every report replaces it; the pill applies it only while its snapshot shows the same
/// epoch and session recording.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Meter {
    pub epoch: SnapshotEpoch,
    pub session: u64,
    pub level: f32,
}

/// Controls for the dictation owner, whose loop runs on `Host`: a dedicated thread, joined on drop
/// once stop has been requested, or a task on the test executor for paused-clock tests.
pub(crate) struct Runtime<Host = OwnedThread> {
    host: Host,
    pub input: InputSender,
    configuration: watch::Sender<Config>,
    reports: Sender<String>,
    stop: Sender<Infallible>,
}

impl Runtime {
    pub(crate) fn start(
        config: ValidConfig,
        snapshots: watch::Sender<Snapshot>,
        meter: watch::Sender<Meter>,
        canvas: watch::Sender<Canvas>,
    ) -> anyhow::Result<(Self, InputMonitor)> {
        let (input, inputs) = input_lane();
        let (monitor, inserter) =
            speakeasy_platform::prepare(input.clone(), config.desktop_options())?;
        let ports = Desktop {
            inserter,
            canvas,
            sounds: Sounds::start()?,
        };
        let runtime = Self::host(
            config.into_inner(),
            snapshots,
            meter,
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
        meter: watch::Sender<Meter>,
        ports: P,
    ) -> anyhow::Result<Self> {
        let (input, inputs) = input_lane();
        Self::host(
            config,
            snapshots,
            meter,
            ports,
            input,
            inputs,
            Wiring::run_on_own_thread,
        )
    }

    pub(crate) fn is_running(&self) -> bool {
        !self.input.is_closed() && !self.host.is_finished()
    }

    /// Resolves once the owner has finished its native cleanup and exited.
    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + use<> {
        self.host.exited()
    }
}

impl<Host> Runtime<Host> {
    /// Wires the owner's lanes and hands its loop to `run`, which decides where it executes.
    fn host<P: Ports>(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
        meter: watch::Sender<Meter>,
        ports: P,
        input: InputSender,
        inputs: Receiver<Input>,
        run: impl FnOnce(Wiring<P>) -> io::Result<Host>,
    ) -> anyhow::Result<Self> {
        let epoch = snapshots.borrow().epoch;
        let (configuration, changes) = watch::channel(config);
        // One report waits for the owner's next wake, and one arriving meanwhile is dropped. The
        // retention sweep repeats its own; a Settings or overlay failure is not repeated.
        let (reports, reported) = async_channel::bounded(1);
        let (stop, stopping) = async_channel::bounded(1);
        let host = run(Wiring {
            changes,
            inputs,
            reports: reported,
            snapshots,
            meter,
            permits: input.clone(),
            stopping,
            ports,
            epoch,
        })?;
        Ok(Self {
            host,
            input,
            configuration,
            reports,
            stop,
        })
    }

    /// Shows `message` as a failure unless a session is under way, which it must not interrupt.
    /// A report already waiting absorbs this one; a stopped owner hands `message` back.
    pub(crate) fn report(&self, message: String) -> Result<(), String> {
        match self.reports.try_send(message) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Closed(message)) => Err(message),
        }
    }

    pub(crate) fn configure(&self, config: ValidConfig) {
        if self.configuration.borrow().change_to(&config) < Change::Session {
            return;
        }
        // Revoke now, so a preparing insertion cannot commit before the owner applies the change.
        self.input.cancel();
        self.configuration.send_replace(config.into_inner());
    }

    /// Wakes the owner on its stop lane before closing input, so a normal pause never reports a
    /// shortcut failure or drains queued presses.
    pub(crate) fn request_stop(&self) {
        self.stop.close();
        self.input.close();
    }
}

impl<Host> Drop for Runtime<Host> {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// The production side effects: native devices, local speech, insertion, Show publishing, and
/// sound cues.
struct Desktop {
    inserter: Inserter,
    /// Where a Show's live ink reaches the overlay.
    canvas: watch::Sender<Canvas>,
    sounds: Sounds,
}

impl Ports for Desktop {
    type Microphone = Capture;
    type Screen = ScreenRecorder;
    type Speech = LocalSpeech;

    fn prepares_desktop(&self) -> bool {
        true
    }

    fn record_microphone(
        &self,
        id: SessionId,
        microphone: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Capture> {
        Capture::start(id, microphone, events)
    }

    fn record_screen(
        &self,
        id: SessionId,
        started: Instant,
        scope: Scope,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<ScreenRecorder> {
        ScreenRecorder::start(id, started, scope, events, self.canvas.clone())
    }

    fn publish(
        &self,
        footage: Footage,
        narration: Narration,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        show::publish(footage, narration, permit)
    }

    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<LocalSpeech>> + Send + 'static {
        LocalSpeech::start(config, cancelled)
    }

    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        delivery: Delivery,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let inserter = self.inserter.clone();
        async move { inserter.insert(text, permit, delivery).await }
    }

    fn cue(&self, cue: Cue) {
        self.sounds.play(cue);
    }

    fn hold_cues(&self, held: bool) {
        self.sounds.hold(held);
    }
}

/// The owner's ends of its lanes, with its ports and the snapshot epoch it presents under.
struct Wiring<P> {
    changes: watch::Receiver<Config>,
    inputs: Receiver<Input>,
    reports: Receiver<String>,
    snapshots: watch::Sender<Snapshot>,
    meter: watch::Sender<Meter>,
    permits: InputSender,
    stopping: Receiver<Infallible>,
    ports: P,
    epoch: SnapshotEpoch,
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

    /// Reports the failure unless the shell has announced a newer snapshot epoch.
    fn fail_startup(&self, error: &io::Error) {
        self.snapshots.send_if_modified(|snapshot| {
            if snapshot.epoch != self.epoch {
                return false;
            }
            snapshot.phase = Phase::Error;
            snapshot.message =
                format!("Could not start dictation ({error}). Quit and reopen Speakeasy.");
            true
        });
    }
}

/// Follows tokio's paused clock in tests, so gesture deadlines, snapshots, and a Show's length
/// share one timeline.
pub(crate) fn now() -> Instant {
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
