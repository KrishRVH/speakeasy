//! The single session authority. Each wake handles one message, advances work whose prerequisites
//! are satisfied, then presents a projection of owned state.

use std::{borrow::Cow, convert::Infallible, ops::ControlFlow, sync::Arc, time::Instant};

use async_channel::{Receiver, RecvError, Sender};
use speakeasy_core::gesture::{Action, Gesture, State};
use speakeasy_platform::{Input, InputSender, Inserted, Scope};
use speakeasy_show::{Ending, Narration, NarrationLoss};
use tokio::{
    sync::watch,
    task::{JoinError, JoinHandle},
};

use super::{
    LOADING, Meter, Phase, Report, SHOW_COPIED, SHOW_GONE, Snapshot, SnapshotEpoch, Wiring,
    device::Device,
    now,
    ports::{CaptureEvent, PUBLISH_STOPPED, Ports, Published, SessionId, ShowEvent, Unchosen},
    reap,
    session::{Delivered, Session},
    worker::{Completion, Detail, Transcript, Worker},
};
use crate::{
    config::{Change, Config},
    show::SavedShow,
    sounds::Cue,
};

/// What a copied Show whose screen stopped recording early tells the user.
const SHOW_SCREEN_STOPPED: &str = "Show copied, but the screen stopped recording early.";
/// What a copied Show whose window closed while it recorded tells the user.
const SHOW_WINDOW_CLOSED: &str = "Show copied, but the recorded window closed.";
/// What a copied Show whose window went off screen while it recorded tells the user.
const SHOW_WINDOW_OFF_SCREEN: &str = "Show copied, but the recorded window went off screen.";
/// What a copied Show whose narration was lost tells the user, before the failure that cost it;
/// its summary tells the agent why.
const SHOW_UNNARRATED: &str = "Show copied without narration.";
/// What a copied Show that lost its narration and whose screen stopped early tells the user. Each
/// loss leaves a different gap, so the message names both, ahead of the failure.
const SHOW_UNNARRATED_SCREEN_STOPPED: &str =
    "Show copied without narration, and the screen stopped recording early.";
/// What a copied Show that lost its narration and whose window closed tells the user.
const SHOW_UNNARRATED_WINDOW_CLOSED: &str =
    "Show copied without narration, and the recorded window closed.";
/// What a copied Show that lost its narration and whose window went off screen tells the user.
const SHOW_UNNARRATED_WINDOW_OFF_SCREEN: &str =
    "Show copied without narration, and the recorded window went off screen.";
/// What a window Show tells the user when no window it can record is under the pointer.
const NO_WINDOW: &str = "No window to record under the pointer, so the Show was cancelled.";
/// What an area Show tells the user when it finished, or could not shade its display, before an
/// area was dragged out.
const NO_AREA: &str = "No area was chosen, so the Show was cancelled.";

/// What the status shows while no session is active: the latest result or failure.
enum Outcome {
    Idle,
    Done,
    /// The dictation is on the clipboard; the message says how to paste it.
    Copied(&'static str),
    /// The Show's summary is on the clipboard; how its screen recording ended and whether its
    /// narration was lost decide the message.
    ShowCopied {
        ending: Ending,
        lost: Option<NarrationLoss>,
    },
    /// The menu found the last Show gone.
    ShowGone,
    Empty,
    Cancelled,
    /// The Show was cancelled because its scope found nothing to record.
    Unchosen(Unchosen),
    /// The pill and Settings show this message as is, so it never includes an error source that
    /// could carry private text, such as a panic payload or engine output.
    Failed(String),
}

impl Outcome {
    const fn phase(&self) -> Phase {
        match self {
            Self::Idle => Phase::Idle,
            Self::Done => Phase::Done,
            Self::Copied(_) | Self::ShowCopied { .. } => Phase::Copied,
            Self::ShowGone => Phase::Notice,
            Self::Empty => Phase::Empty,
            Self::Cancelled => Phase::Cancelled,
            Self::Unchosen(_) => Phase::Unchosen,
            Self::Failed(_) => Phase::Error,
        }
    }
}

/// Status text shown unless the outcome carries its own message: a failure or a copied delivery.
/// A Show copied without narration still shows the failure it outlived, after its own message.
enum Notice {
    LoadingModel,
    SettingsApplied,
    /// The model failed to load; a later load clears it.
    WarmupFailed(String),
    /// Why audio capture or transcription cost a Show its narration, with the remedy. A later load
    /// leaves it, since the cause may lie outside the model.
    NarrationLost(String),
}

impl Notice {
    fn text(&self) -> &str {
        match self {
            Self::LoadingModel => LOADING,
            Self::SettingsApplied => "Settings applied",
            Self::WarmupFailed(failure) | Self::NarrationLost(failure) => failure,
        }
    }

    /// The failure a session outlived, if this notice is one.
    fn failure(&self) -> Option<&str> {
        match self {
            Self::WarmupFailed(failure) | Self::NarrationLost(failure) => Some(failure),
            Self::LoadingModel | Self::SettingsApplied => None,
        }
    }
}

enum Wake<W> {
    Stop,
    Input(Result<Input, RecvError>),
    Report(Report),
    Configuration,
    Capture(CaptureEvent),
    Show(ShowEvent),
    Worker(Completion<W>),
    Delivered(Result<Delivered, JoinError>),
    Retired,
    Timer,
}

/// The latest session as the snapshot presents it, during the session and after it ends.
struct Latest {
    id: SessionId,
    started: Instant,
    /// Whether it became a Show, which the pill presents differently.
    show: bool,
}

struct Owner<P: Ports> {
    ports: P,
    config: Config,

    changes: watch::Receiver<Config>,
    inputs: Receiver<Input>,
    reports: Receiver<Report>,
    stopping: Receiver<Infallible>,
    capture_events: Sender<CaptureEvent>,
    captures: Receiver<CaptureEvent>,
    show_events: Sender<ShowEvent>,
    shows: Receiver<ShowEvent>,

    permits: InputSender,
    gesture: Gesture,
    session: Option<Session>,
    latest: Latest,
    /// The latest Show saved, which only a later saved Show replaces and only a report that finds
    /// it gone forgets; an abandoned publication's result is never awaited, so it cannot.
    last_show: Option<SavedShow>,
    /// How many reports have shown since this owner started.
    reported: u64,

    worker: Worker<P::Speech>,
    microphone: Device<P::Microphone>,
    screen: Device<P::Screen>,
    /// Abandoned insertions and publications, kept until they finish their own cleanup.
    retiring: Vec<JoinHandle<Delivered>>,
    /// Whether the cue output is held open, as it is through every session while sounds are on.
    cues_held: bool,

    snapshots: watch::Sender<Snapshot>,
    meter: watch::Sender<Meter>,
    epoch: SnapshotEpoch,
    outcome: Outcome,
    notice: Option<Notice>,
    desktop_ready: bool,
    shortcut: Arc<str>,
    cancel_shortcut: Arc<str>,
}

impl<P: Ports> Owner<P> {
    fn new(wiring: Wiring<P>) -> Self {
        let Wiring {
            mut changes,
            inputs,
            reports,
            snapshots,
            meter,
            permits,
            stopping,
            ports,
            epoch,
            last_show,
        } = wiring;
        let config = changes.borrow_and_update().clone();
        let (capture_events, captures) = async_channel::bounded(64);
        let (show_events, shows) = async_channel::bounded(4);
        let desktop_ready = !ports.prepares_desktop();
        let worker = Worker::load(&ports, &config);
        Self {
            ports,
            config,
            changes,
            inputs,
            reports,
            stopping,
            capture_events,
            captures,
            show_events,
            shows,
            permits,
            gesture: Gesture::default(),
            session: None,
            latest: Latest {
                id: SessionId::default(),
                started: now(),
                show: false,
            },
            last_show,
            reported: 0,
            worker,
            microphone: Device::Free,
            screen: Device::Free,
            retiring: Vec::new(),
            cues_held: false,
            snapshots,
            meter,
            epoch,
            outcome: Outcome::Idle,
            notice: Some(Notice::LoadingModel),
            desktop_ready,
            shortcut: speakeasy_platform::SHORTCUT.into(),
            cancel_shortcut: speakeasy_platform::CANCEL_SHORTCUT.into(),
        }
    }

    async fn wait(&mut self) -> Wake<P::Speech> {
        let timer = or_pending(
            self.gesture
                .deadline()
                .map(|at| tokio::time::sleep_until(at.into())),
        );
        let delivered = or_pending(self.session.as_mut().map(Session::delivered));
        // Biased: stop outranks the input lane that `request_stop` closes beside it, so a pause
        // never reads as a shortcut failure.
        tokio::select! {
            biased;
            _ = self.stopping.recv() => Wake::Stop,
            input = self.inputs.recv() => Wake::Input(input),
            // Only `Runtime` sends reports and changes, and dropping it stops first.
            report = self.reports.recv() => report.map_or(Wake::Stop, Wake::Report),
            change = self.changes.changed() => change.map_or(Wake::Stop, |()| Wake::Configuration),
            Ok(event) = self.captures.recv() => Wake::Capture(event),
            Ok(event) = self.shows.recv() => Wake::Show(event),
            () = self.microphone.retired() => Wake::Retired,
            () = self.screen.retired() => Wake::Retired,
            completion = self.worker.completed() => Wake::Worker(completion),
            delivered = delivered => Wake::Delivered(delivered),
            () = timer => Wake::Timer,
        }
    }

    fn handle(&mut self, wake: Wake<P::Speech>) -> ControlFlow<()> {
        let action = match wake {
            Wake::Stop => return ControlFlow::Break(()),
            Wake::Input(input) => self.handle_input(input)?,
            Wake::Report(report) => {
                self.report(report);
                None
            },
            Wake::Configuration => {
                self.configure();
                None
            },
            Wake::Capture(event) => {
                self.handle_capture(event);
                None
            },
            Wake::Show(event) => self.handle_show(event),
            Wake::Retired => None,
            Wake::Worker(completion) => {
                self.handle_worker(completion);
                None
            },
            Wake::Delivered(result) => {
                self.handle_delivered(result);
                None
            },
            Wake::Timer => self.gesture.tick(now()),
        };
        self.handle_action(action)
    }

    fn handle_input(&mut self, input: Result<Input, RecvError>) -> ControlFlow<(), Option<Action>> {
        let Ok(input) = input else {
            return self.stop_with_failure(
                "Speakeasy stopped watching the dictation shortcut. Resume dictation to try again."
                    .into(),
            );
        };
        let action = match input {
            Input::Press if self.desktop_ready => self.gesture.press(now()),
            Input::Toggle if self.desktop_ready => self.gesture.toggle(now()),
            Input::Press | Input::Toggle => None,
            Input::Release => self.gesture.release(now()),
            Input::Lock => {
                self.gesture.lock();
                None
            },
            Input::Show(scope) => {
                self.start_show(scope);
                None
            },
            Input::Pin(taps) => {
                self.screen.pin(taps);
                None
            },
            Input::Escape => self.gesture.escape(),
            Input::Cancel => self.gesture.cancel(),
            // Unlike `Cancel`, no cue: the user never asked to cancel.
            Input::Interrupt => {
                if self.gesture.cancel().is_some() {
                    self.discard();
                }
                None
            },
            Input::Unavailable(message) => return self.stop_with_failure(message),
            Input::DesktopReady { shortcut, cancel } => {
                self.desktop_ready = true;
                self.shortcut = shortcut.into();
                self.cancel_shortcut = cancel.into();
                None
            },
        };
        ControlFlow::Continue(action)
    }

    fn stop_with_failure<T>(&mut self, message: String) -> ControlFlow<(), T> {
        self.fail(message);
        self.present();
        ControlFlow::Break(())
    }

    /// Shows `report` as [`Report::present`] describes, counting the menu's so that a repeat
    /// presents anew.
    fn report(&mut self, report: Report) {
        report.forget_gone(&mut self.last_show);
        // The owner's own state, not a presented copy, decides idleness, so a session that started
        // meanwhile is never overwritten.
        if self.session.is_some() {
            return;
        }
        if report.answers_the_menu() {
            self.reported = self.reported.wrapping_add(1);
        }
        self.outcome = match report {
            Report::Failed(message) => Outcome::Failed(message),
            Report::ShowCopied => Outcome::ShowCopied {
                ending: Ending::Finished,
                lost: None,
            },
            Report::ShowGone(_) => Outcome::ShowGone,
        };
    }

    /// Applies changed settings, which end any session in progress: it shows cancelled, with its
    /// cue, so its speech never vanishes unannounced.
    fn configure(&mut self) {
        let next = self.changes.borrow_and_update().clone();
        let reload = self.config.change_to(&next) >= Change::Engine;
        self.config = next;
        let cancelled = self.gesture.abandon().is_some();
        if reload {
            self.worker.reload(&self.ports, &self.config);
        }
        self.abandon();
        self.worker.revive(&self.ports, &self.config);
        self.outcome = if cancelled {
            self.cue(Cue::Cancel);
            Outcome::Cancelled
        } else {
            Outcome::Idle
        };
        self.notice = Some(if self.worker.is_ready() {
            Notice::SettingsApplied
        } else {
            Notice::LoadingModel
        });
    }

    fn handle_capture(&mut self, event: CaptureEvent) {
        let gesture = self.gesture.state();
        let Some(session) = self.current_capture_mut(event.session()) else {
            return;
        };
        match event {
            CaptureEvent::Ready(_) => {
                session.audio_ready(gesture, now());
                // The pill opens to listening here, so the cue sounds with it, never before the
                // microphone listens.
                let cue = if session.is_show() {
                    Cue::Show
                } else {
                    Cue::Start
                };
                if session.is_recording() {
                    self.cue_heard(cue);
                }
            },
            CaptureEvent::Level(id, level) => {
                self.meter.send_replace(Meter {
                    epoch: self.epoch,
                    session: id.into(),
                    level,
                });
            },
            CaptureEvent::Finished(_, Ok(sealed)) => {
                let delivers = session.captured(sealed);
                self.conclude_capture();
                if !delivers {
                    self.end_session(Outcome::Empty);
                }
            },
            CaptureEvent::Finished(_, Err(error)) => {
                if session.audio_stopped() {
                    // A Show whose microphone listened ends where its audio stopped, keeping its
                    // footage and the failure's remedy.
                    self.notice = Some(Notice::NarrationLost(error.to_string()));
                    self.conclude_capture();
                } else {
                    // Failing retires both devices, so the screen is never asked to finish footage
                    // for nobody.
                    self.fail(error.to_string());
                }
            },
        }
    }

    /// The only way a capture event reaches a session: the identity must match and the session must
    /// still be capturing, so late audio from a completed capture is ignored.
    fn current_capture_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.session
            .as_mut()
            .filter(|session| session.id == id && session.is_capturing())
    }

    /// Ends capture however it ended, finishing a Show's screen with its narration. A capture that
    /// ended before the gesture did sounds the stop cue the gesture would have.
    fn conclude_capture(&mut self) {
        self.microphone.retire();
        self.screen.finish();
        if self.gesture.finish().is_some() {
            self.cue(Cue::Stop);
        }
    }

    fn handle_worker(&mut self, completion: Completion<P::Speech>) {
        match completion {
            Completion::Loaded(Ok(worker)) => {
                self.worker = Worker::Ready(worker);
                if matches!(
                    self.notice,
                    Some(Notice::LoadingModel | Notice::WarmupFailed(_))
                ) {
                    self.notice = None;
                }
            },
            Completion::Loaded(Err(error)) => {
                if self.session_survives_warmup_failure() {
                    // A Show that already lost its narration keeps that failure in view; the
                    // snapshot's model state shows the load failed.
                    if !matches!(self.notice, Some(Notice::NarrationLost(_))) {
                        self.notice = Some(Notice::WarmupFailed(error.to_string()));
                    }
                } else if self.session.as_mut().is_some_and(Session::not_transcribed) {
                    // A Show waiting for the worker goes on without narration, the failure in view.
                    self.notice = Some(Notice::WarmupFailed(error.to_string()));
                } else {
                    self.fail(error.to_string());
                }
            },
            Completion::Transcribed(Ok((worker, transcript))) => {
                self.worker = Worker::Ready(worker);
                match transcript {
                    Transcript::Text(text) => self.insert_transcript(text),
                    Transcript::Words(narration) => self.narrate(narration),
                }
            },
            Completion::Transcribed(Err(error)) => {
                // The session still holds its speech, which `advance` resends once a fresh worker
                // loads. After that one retry, a Show goes on without narration.
                if !self.session.as_mut().is_some_and(Session::retry) {
                    if self.session.as_mut().is_some_and(Session::not_transcribed) {
                        self.notice = Some(Notice::NarrationLost(error.to_string()));
                    } else {
                        self.fail(error.to_string());
                    }
                    self.worker.revive(&self.ports, &self.config);
                }
            },
        }
    }

    /// A session that is not waiting for the worker survives a failed warmup: its gesture finishes
    /// on its own terms, and an owned microphone must be released before a retry may start.
    fn session_survives_warmup_failure(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| !session.awaits_worker())
    }

    /// Only the current job's transcript arrives here: abandoning a session first moves its job out
    /// of observation, so stale text can never be inserted.
    fn insert_transcript(&mut self, text: String) {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.is_transcribing())
        else {
            return;
        };
        if text.is_empty() {
            self.end_session(Outcome::Empty);
        } else {
            session.insert(|permit| self.ports.insert(text, permit, self.config.delivery()));
        }
    }

    /// Only the current Show's narration arrives here, for the same reason as a transcript.
    fn narrate(&mut self, narration: Narration) {
        if let Some(session) = &mut self.session {
            session.narrate(narration);
        }
    }

    /// Turns the held recording into a Show of what `scope` names, keeping the narration already
    /// captured, and records it hands-free until the shortcut is pressed and released again. The
    /// keyboard policy sends a Show only for a press during a physical hold, and its repeats, so a
    /// recording already locked hands-free with Space or a double tap turns too, as does one still
    /// waiting for the microphone.
    fn start_show(&mut self, scope: Scope) {
        let held = matches!(self.gesture.state(), State::Held | State::HandsFree);
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if !held || !session.turn_into_show(scope) {
            return;
        }
        self.latest.show = true;
        // A microphone still opening sounds the Show cue once it listens.
        if session.is_recording() {
            self.cue_heard(Cue::Show);
        }
        self.gesture.show();
    }

    /// Keeps a Show's footage. A screen that stopped before the gesture finished, as a capture
    /// failing partway does, finishes the Show with it. A Show whose scope found nothing to record
    /// is cancelled with its narration, since a Show never records more than its scope names.
    fn handle_show(&mut self, event: ShowEvent) -> Option<Action> {
        let session = self
            .session
            .as_mut()
            .filter(|session| session.id == event.session() && session.is_show())?;
        let footage = match event {
            ShowEvent::Finished(_, footage) => footage,
            ShowEvent::Unchosen(_, unchosen) => {
                self.gesture.abandon();
                self.discard();
                self.cue(Cue::Cancel);
                self.outcome = Outcome::Unchosen(unchosen);
                return None;
            },
        };
        match footage {
            Ok(footage) => {
                session.screen_recorded(footage);
                self.screen.retire();
                self.gesture.finish()
            },
            Err(error) => {
                self.fail(format!("{error:#}"));
                None
            },
        }
    }

    fn handle_delivered(&mut self, result: Result<Delivered, JoinError>) {
        // Only a Show publishes, and a session cannot become one once transcription starts.
        let show = self.session.as_ref().is_some_and(Session::is_show);
        let outcome = match result {
            Ok(Delivered::Inserted(Ok(Inserted::Sent))) => Outcome::Done,
            Ok(Delivered::Inserted(Ok(Inserted::Copied(message)))) => Outcome::Copied(message),
            Ok(Delivered::Published(Ok(Published::Saved {
                show: saved,
                ending,
                lost,
                copied,
            }))) => {
                self.remember(saved);
                match copied {
                    Ok(()) => Outcome::ShowCopied { ending, lost },
                    Err(error) => Outcome::Failed(format!(
                        "The Show was saved but not copied ({error:#}). Choose Copy last Show in \
                         Speakeasy's menu."
                    )),
                }
            },
            Ok(
                Delivered::Inserted(Ok(Inserted::Cancelled))
                | Delivered::Published(Ok(Published::Cancelled)),
            ) => Outcome::Cancelled,
            Ok(Delivered::Inserted(Ok(Inserted::Unavailable(message)))) => {
                Outcome::Failed(message.into())
            },
            Ok(Delivered::Published(Err(error))) => Outcome::Failed(format!("{error:#}")),
            Err(_) if show => Outcome::Failed(PUBLISH_STOPPED.into()),
            Ok(Delivered::Inserted(Err(_))) | Err(_) => Outcome::Failed(
                "Cannot submit the text. Check desktop permissions and clipboard access, then try again."
                    .into(),
            ),
        };
        if matches!(outcome, Outcome::Copied(_) | Outcome::ShowCopied { .. }) {
            self.cue(Cue::Copied);
        }
        // `wait` already joined this delivery, so the session holds no work left to retire.
        self.end_session(outcome);
    }

    /// Remembers `show` as the last Show. A pause may already have announced over this owner,
    /// whose presentations the epoch then drops, yet the Show outlives it: the announced snapshot
    /// takes it too, so the next owner starts from it.
    fn remember(&mut self, show: SavedShow) {
        // Written straight into the presented copy, so a pause announced before this owner next
        // presents still keeps it; only a stale copy, which nothing else updates, notifies.
        self.snapshots.send_if_modified(|presented| {
            presented.last_show = Some(show.clone());
            presented.epoch != self.epoch
        });
        self.last_show = Some(show);
    }

    fn handle_action(&mut self, action: Option<Action>) -> ControlFlow<()> {
        match action {
            Some(Action::Start) => return self.start_session(),
            Some(Action::Finish) => {
                self.microphone.finish();
                self.screen.finish();
                if let Some(session) = &mut self.session {
                    session.finish();
                }
                // The microphone has stopped taking samples, so it cannot hear this.
                self.cue(Cue::Stop);
            },
            Some(Action::Cancel) => {
                self.discard();
                self.cue(Cue::Cancel);
            },
            None => {},
        }
        ControlFlow::Continue(())
    }

    fn start_session(&mut self) -> ControlFlow<()> {
        // A closed lane still holds whatever closed it: the next wake reads a queued
        // `Unavailable`, the stop lane, or the drained lane's error through `handle_input`.
        let Some(permit) = self.permits.begin() else {
            self.gesture.abandon();
            return ControlFlow::Continue(());
        };
        let Some(id) = self.latest.id.next() else {
            return ControlFlow::Break(());
        };
        self.latest = Latest {
            id,
            started: now(),
            show: false,
        };
        self.outcome = Outcome::Idle;
        self.notice = None;
        self.session = Some(Session::new(id, permit, self.latest.started));
        ControlFlow::Continue(())
    }

    /// Holds the cue output open from the moment a session begins, so its first cue never waits
    /// for a device to open, and releases it as Speakeasy goes idle or sounds turn off, however the
    /// session ended.
    fn hold_cues(&mut self) {
        let held = self.config.play_sounds && self.session.is_some();
        if held != self.cues_held {
            self.cues_held = held;
            self.ports.hold_cues(held);
        }
    }

    /// Sounds `cue` while the user wants sounds.
    fn cue(&self, cue: Cue) {
        if self.config.play_sounds {
            self.ports.cue(cue);
        }
    }

    /// Sounds `cue` where the listening microphone can hear it, telling the microphone first so
    /// the speech gate listens beneath it.
    fn cue_heard(&self, cue: Cue) {
        if self.config.play_sounds {
            self.microphone.expect_cue();
            self.ports.cue(cue);
        }
    }

    /// Ends a session whose work has finished, so it holds nothing left to retire, and shows
    /// `outcome`.
    fn end_session(&mut self, outcome: Outcome) {
        if let Some(session) = self.session.take() {
            session.permit.revoke();
        }
        self.gesture.complete();
        self.outcome = outcome;
    }

    /// Discards the session, its insertion and any transcription under way, and shows it cancelled.
    fn discard(&mut self) {
        self.abandon();
        self.worker.revive(&self.ports, &self.config);
        self.outcome = Outcome::Cancelled;
    }

    /// Revokes the session and hands its native work to cleanup without awaiting any OS call.
    fn abandon(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        session.permit.revoke();
        if session.is_show() {
            self.screen.retire();
        }
        if session.is_capturing() {
            self.microphone.retire();
        }
        if session.is_transcribing() && self.worker.is_transcribing() {
            self.worker.recover(&self.ports, &self.config);
        }
        if let Some(task) = session.into_delivery() {
            self.retiring.push(task);
        }
    }

    fn fail(&mut self, message: String) {
        self.gesture.abandon();
        self.abandon();
        self.outcome = Outcome::Failed(message);
    }

    fn advance(&mut self) {
        self.open_queued_capture();
        self.open_queued_screen();
        self.start_transcription();
        self.start_publishing();
        self.reap_retired_work();
    }

    fn open_queued_capture(&mut self) {
        if self.permits.is_closed() || !self.microphone.is_free() {
            return;
        }
        let Some(session) = self.session.as_mut().filter(|session| session.is_queued()) else {
            return;
        };
        let microphone = self.config.microphone.as_deref();
        match self
            .ports
            .record_microphone(session.id, microphone, self.capture_events.clone())
        {
            Ok(recording) => {
                self.microphone.open(recording, self.gesture.state());
                session.opened();
            },
            Err(error) => self.fail(format!("Cannot open the microphone ({error}). Try again.")),
        }
    }

    /// Opens a Show's screen recorder once any previous one has retired, even after its narration
    /// ended, so the Show still gets the screen as it was then.
    fn open_queued_screen(&mut self) {
        if self.permits.is_closed() || !self.screen.is_free() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(scope) = session.queued_screen() else {
            return;
        };
        match self
            .ports
            .record_screen(session.id, session.started, scope, self.show_events.clone())
        {
            Ok(recorder) => {
                self.screen.open(recorder, self.gesture.state());
                session.screen_opened();
            },
            Err(error) => self.fail(format!("Cannot record the screen: {error}")),
        }
    }

    fn start_transcription(&mut self) {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.awaits_worker())
        else {
            return;
        };
        let detail = if session.is_show() {
            Detail::Words
        } else {
            Detail::Text
        };
        if self.worker.revive(&self.ports, &self.config) {
            self.notice = Some(Notice::LoadingModel);
        }
        session.transcribe(|wav| self.worker.transcribe(wav, detail, &self.config));
    }

    /// Publishes a Show once its narration and screen recording are both in.
    fn start_publishing(&mut self) {
        if let Some(session) = &mut self.session {
            session.publish(|footage, narration, lost, permit| {
                self.ports.publish(footage, narration, lost, permit)
            });
        }
    }

    /// Obsolete insertions and publications finish their own cleanup; they are never aborted, and
    /// their results cannot reach the current session. A finished one is dropped with its result;
    /// an unfinished one stays, because dropping its handle would detach it from shutdown.
    fn reap_retired_work(&mut self) {
        self.retiring.retain(|task| !task.is_finished());
    }

    fn message(&self) -> Cow<'_, str> {
        match &self.outcome {
            Outcome::Failed(message) => message.into(),
            Outcome::Copied(message) => Cow::Borrowed(message),
            Outcome::ShowCopied { ending, lost: None } => match ending {
                Ending::Finished => SHOW_COPIED,
                Ending::ScreenStopped => SHOW_SCREEN_STOPPED,
                Ending::WindowClosed => SHOW_WINDOW_CLOSED,
                Ending::WindowOffScreen => SHOW_WINDOW_OFF_SCREEN,
            }
            .into(),
            Outcome::ShowCopied {
                ending,
                lost: Some(_),
            } => {
                let copied = match ending {
                    Ending::Finished => SHOW_UNNARRATED,
                    Ending::ScreenStopped => SHOW_UNNARRATED_SCREEN_STOPPED,
                    Ending::WindowClosed => SHOW_UNNARRATED_WINDOW_CLOSED,
                    Ending::WindowOffScreen => SHOW_UNNARRATED_WINDOW_OFF_SCREEN,
                };
                match self.notice.as_ref().and_then(Notice::failure) {
                    Some(failure) => format!("{copied} {failure}").into(),
                    None => copied.into(),
                }
            },
            Outcome::Unchosen(Unchosen::Window) => NO_WINDOW.into(),
            Outcome::Unchosen(Unchosen::Area) => NO_AREA.into(),
            Outcome::ShowGone => SHOW_GONE.into(),
            Outcome::Idle | Outcome::Done | Outcome::Empty | Outcome::Cancelled => {
                self.notice.as_ref().map_or("", Notice::text).into()
            },
        }
    }

    fn present(&self) {
        debug_assert_eq!(self.gesture.state() == State::Idle, self.session.is_none());
        let session = self.session.as_ref();
        let (ending, narration_lost) = match self.outcome {
            Outcome::ShowCopied { ending, lost } => (ending, lost),
            _ => (Ending::Finished, None),
        };
        let snapshot = Snapshot {
            epoch: self.epoch,
            session: self.latest.id.into(),
            reported: self.reported,
            phase: session.map_or_else(|| self.outcome.phase(), Session::phase),
            hands_free: self.gesture.state() == State::HandsFree,
            // A Show copied again reads as one just copied, whatever the latest session was.
            show: self.latest.show || matches!(self.outcome, Outcome::ShowCopied { .. }),
            ending,
            narration_lost,
            started: self.latest.started,
            last_show: self.last_show.clone(),
            message: self.message().into_owned(),
            model: self.worker.model(),
            desktop_ready: self.desktop_ready,
            shortcut: Arc::clone(&self.shortcut),
            cancel_shortcut: Arc::clone(&self.cancel_shortcut),
        };
        self.snapshots.send_if_modified(|presented| {
            if presented.epoch != self.epoch || *presented == snapshot {
                return false;
            }
            *presented = snapshot;
            true
        });
    }

    async fn shutdown(mut self) {
        self.permits.close();
        // Nothing reads the capture lane from here on, and a capture thread reports `Finished`
        // with a blocking send even when cancelled, so a full lane would stall its teardown.
        self.captures.close();
        self.abandon();
        let retired = async {
            for task in self.retiring {
                reap(task).await;
            }
        };
        tokio::join!(
            self.worker.stop(),
            self.microphone.stop(),
            self.screen.stop(),
            retired
        );
    }
}

pub(super) async fn run<P: Ports>(wiring: Wiring<P>) {
    let mut owner = Owner::new(wiring);
    owner.present();
    loop {
        let wake = owner.wait().await;
        if owner.handle(wake).is_break() {
            break;
        }
        owner.advance();
        owner.hold_cues();
        owner.present();
    }
    owner.shutdown().await;
}

/// Pends unless a future is present; resolves once it completes.
async fn or_pending<F: Future>(future: Option<F>) -> F::Output {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}
