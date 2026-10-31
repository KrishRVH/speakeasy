//! Thread ownership, production ports, and presentation interface for the dictation owner. Stop
//! revokes authority immediately; `stopped` acknowledges owned native cleanup.

mod device;
mod owner;
pub(crate) mod ports;
mod recognition;
mod session;
mod worker;

use std::{convert::Infallible, io, sync::Arc, time::Instant};

use async_channel::{Receiver, Sender, TrySendError};
use speakeasy_platform::{
    Delivery, Input, InputMonitor, InputSender, InsertPermit, Inserted, Inserter, OwnedThread,
    Scope,
};
use speakeasy_show::{Ending, NarrationLoss};
use tokio::{
    sync::{mpsc, watch},
    task::JoinError,
};
use zeroize::Zeroizing;

use self::ports::{CaptureEvent, Ports, Published, SessionId, ShowEvent};
use crate::{
    audio::Capture,
    config::{Change, Config, ValidConfig},
    local_speech::LocalSpeech,
    show::{self, Canvas, Interrupted, SavedShow, ScreenRecorder},
    sounds::{Cue, Sounds},
};

/// The status while a speech worker loads, shared by the owner, Settings, and the tray.
pub(crate) const LOADING: &str = "Loading local model…";
/// What a copied Show tells the user, whether it was just published or copied again from the menu.
const SHOW_COPIED: &str = "Show copied. Paste it into your coding agent.";
/// What the menu reports once the last Show's folder has gone.
const SHOW_GONE: &str = "The last Show has expired or was deleted.";

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
    /// Something the user should know that needs no action, such as the last Show having gone;
    /// `message` says it.
    Notice,
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
    /// How many reports from outside a session have shown, so one that repeats the last still reads
    /// as new; a recurring failure reads as new only when it says something new.
    pub reported: u64,
    pub phase: Phase,
    pub hands_free: bool,
    /// Whether the latest session became a Show, recording the screen as well as the microphone,
    /// or the result shown is the last Show copied again.
    pub show: bool,
    /// How the latest Show's screen recording ended once the Show is copied, as its message says;
    /// `Finished` before then and for dictation.
    pub ending: Ending,
    /// Why the latest Show's narration was lost once the Show is copied, as its message says;
    /// `None` before then, for dictation, and for a Show whose narration was heard.
    pub narration_lost: Option<NarrationLoss>,
    /// When the latest session started; the pill's recording clock counts from it.
    pub started: Instant,
    /// The latest Show saved since Speakeasy started, which the menu can copy or open again. It
    /// outlives each owner: the shell's announcements keep it and a new owner starts from it, so
    /// only quitting forgets it.
    pub last_show: Option<SavedShow>,
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
            reported: 0,
            phase: Phase::Idle,
            hands_free: false,
            show: false,
            ending: Ending::Finished,
            narration_lost: None,
            started: now(),
            last_show: None,
            message: String::new(),
            model: ModelState::Unavailable,
            desktop_ready: true,
            shortcut: speakeasy_platform::SHORTCUT.into(),
            cancel_shortcut: speakeasy_platform::CANCEL_SHORTCUT.into(),
        }
    }
}

/// A result from outside any session, which is shown only while no session is under way.
pub(crate) enum Report {
    /// A failure that met the user's request, such as the menu's or opening Settings, shown as its
    /// message.
    Failed(String),
    /// A failure a background job reports at each attempt until it is fixed, such as a retention
    /// sweep's, shown as its message.
    Recurring(String),
    /// The last Show's summary went on the clipboard again.
    #[cfg_attr(
        all(target_os = "linux", not(test)),
        expect(
            dead_code,
            reason = "Linux records no Shows, so its menu never copies one"
        )
    )]
    ShowCopied,
    /// The menu found this Show's folder gone, expired or deleted.
    #[cfg_attr(
        all(target_os = "linux", not(test)),
        expect(
            dead_code,
            reason = "Linux records no Shows, so its menu never looks for one"
        )
    )]
    ShowGone(SavedShow),
}

impl Report {
    /// Applies this report to `snapshot` for when no owner presents one, returning whether it
    /// changed. It shows unless a session is under way, which it must not interrupt, and a Show it
    /// found gone is forgotten either way.
    pub(crate) fn present(self, snapshot: &mut Snapshot) -> bool {
        let shown = snapshot.clone();
        self.forget_gone(&mut snapshot.last_show);
        if snapshot.phase.is_active() {
            return *snapshot != shown;
        }
        if self.presents_anew() {
            snapshot.reported = snapshot.reported.wrapping_add(1);
        }
        (snapshot.phase, snapshot.message) = match self {
            Self::Failed(message) | Self::Recurring(message) => (Phase::Error, message),
            Self::ShowCopied => {
                // A Show copied again reads as one just copied, whatever the latest session was.
                (snapshot.show, snapshot.ending, snapshot.narration_lost) =
                    (true, Ending::Finished, None);
                (Phase::Copied, SHOW_COPIED.to_owned())
            },
            Self::ShowGone(_) => (Phase::Notice, SHOW_GONE.to_owned()),
        };
        *snapshot != shown
    }

    /// Whether this report shows anew even when it repeats the last, as an answer to the user does.
    /// A recurring failure shows again only when it changes, so a sweep cannot pop it up hourly.
    fn presents_anew(&self) -> bool {
        !matches!(self, Self::Recurring(_))
    }

    /// Forgets `last_show` if this report found that same Show gone; a newer Show saved meanwhile
    /// stays.
    fn forget_gone(&self, last_show: &mut Option<SavedShow>) {
        if let Self::ShowGone(gone) = self
            && last_show.as_ref() == Some(gone)
        {
            *last_show = None;
        }
    }
}

impl From<&anyhow::Error> for Report {
    /// Keeps only the outermost message, which says what failed and what to do.
    fn from(error: &anyhow::Error) -> Self {
        Self::Failed(error.to_string())
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
    reports: Sender<Report>,
    recoveries: Sender<Recovery>,
    practices: Sender<mpsc::Sender<PracticeEvent>>,
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
            mac_shortcut: config.mac_shortcut,
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
        let (epoch, last_show) = {
            let presented = snapshots.borrow();
            (presented.epoch, presented.last_show.clone())
        };
        let (configuration, changes) = watch::channel(config);
        // One report waits for the owner's next wake, and one arriving meanwhile is dropped. The
        // retention sweep repeats its own; a Settings or overlay failure is not repeated.
        let (reports, reported) = async_channel::bounded(1);
        let (stop, stopping) = async_channel::bounded(1);
        let (recoveries, recovering) = async_channel::bounded(1);
        let (practices, practicing) = async_channel::bounded(1);
        let host = run(Wiring {
            changes,
            inputs,
            reports: reported,
            recoveries: recovering,
            practices: practicing,
            snapshots,
            meter,
            permits: input.clone(),
            stopping,
            ports,
            epoch,
            last_show,
        })?;
        Ok(Self {
            host,
            input,
            configuration,
            reports,
            recoveries,
            practices,
            stop,
        })
    }

    /// Shows `report` unless a session is under way, which it must not interrupt, as
    /// [`Report::present`] describes. A report already waiting absorbs this one; a stopped owner
    /// hands `report` back.
    pub(crate) fn report(&self, report: Report) -> Result<(), Report> {
        match self.reports.try_send(report) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Closed(report)) => Err(report),
        }
    }

    /// Queues one explicit recovery. The owner waits for its current session and owns the result.
    pub(crate) fn recover(
        &self,
        interrupted: Interrupted,
    ) -> Result<RecoveryReceipt, &'static str> {
        let (completed, result) = async_channel::bounded(1);
        self.recoveries
            .try_send(Recovery {
                interrupted,
                completed,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => "A Show recovery is already waiting.",
                TrySendError::Closed(_) => "Resume Speakeasy to recover this Show.",
            })?;
        Ok(result)
    }

    /// Arms private practice after the owner acknowledges `Ready`. Dropping the receipt revokes
    /// practice; text is never sent to the focused application or a shared status snapshot.
    pub(crate) fn practice(&self) -> Result<mpsc::Receiver<PracticeEvent>, &'static str> {
        let (sender, receipt) = mpsc::channel(2);
        self.practices
            .try_send(sender)
            .map_err(|error| match error {
                TrySendError::Full(_) => "Practice is already starting.",
                TrySendError::Closed(_) => "Resume dictation to try it here.",
            })?;
        Ok(receipt)
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
    mac_shortcut: speakeasy_platform::MacShortcut,
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
        recognition: crate::runtime::ports::RecognitionMode,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Capture> {
        Capture::start(id, microphone, recognition, events)
    }

    fn keep(&self, recorded: std::time::SystemTime) -> anyhow::Result<Option<show::Kept>> {
        show::keep(recorded).map(Some)
    }

    fn record_screen(
        &self,
        id: SessionId,
        started: Instant,
        scope: Scope,
        log: Option<show::FrameLog>,
        events: Sender<ShowEvent>,
    ) -> anyhow::Result<ScreenRecorder> {
        ScreenRecorder::start(
            id,
            started,
            scope,
            log,
            events,
            self.canvas.clone(),
            self.mac_shortcut,
        )
    }

    fn publish(
        &self,
        publication: show::Publication,
        permit: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        show::publish(publication, permit)
    }

    fn reopen(
        &self,
        interrupted: Interrupted,
    ) -> impl Future<Output = anyhow::Result<show::Reopened>> + Send + 'static {
        show::reopen(interrupted)
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

/// Completion of an accepted recovery; dropping the receipt does not cancel owned publication.
pub(crate) type RecoveryReceipt = Receiver<Result<SavedShow, String>>;

/// Private, transient practice output. No implementation of `Debug` exposes recognized words.
pub(crate) enum PracticeEvent {
    Ready,
    Text(Zeroizing<String>),
    Unavailable(&'static str),
}

struct Recovery {
    interrupted: Interrupted,
    completed: Sender<Result<SavedShow, String>>,
}

/// The owner's lane ends, ports, presentation epoch, and latest saved Show.
struct Wiring<P> {
    changes: watch::Receiver<Config>,
    inputs: Receiver<Input>,
    reports: Receiver<Report>,
    recoveries: Receiver<Recovery>,
    practices: Receiver<mpsc::Sender<PracticeEvent>>,
    snapshots: watch::Sender<Snapshot>,
    meter: watch::Sender<Meter>,
    permits: InputSender,
    stopping: Receiver<Infallible>,
    ports: P,
    epoch: SnapshotEpoch,
    last_show: Option<SavedShow>,
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
