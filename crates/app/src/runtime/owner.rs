use std::{
    convert::Infallible, future::poll_fn, mem, ops::ControlFlow, pin::Pin, sync::Arc, task::Poll,
    time::Instant,
};

use async_channel::{Receiver, RecvError, Sender};
use speakeasy_core::gesture::{Action, Gesture, State};
use speakeasy_platform::{Input, InputSender, Inserted, Scope};
use speakeasy_show::{Ending, NarrationLoss};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinError, JoinHandle},
};

use super::{
    LOADING, Meter, ModelState, Phase, PracticeEvent, Recovery, Report, SHOW_COPIED, SHOW_GONE,
    Snapshot, SnapshotEpoch, Wiring,
    device::Device,
    now,
    ports::{
        CaptureEvent, PUBLISH_STOPPED, Ports, Preservation, Published, SessionId, ShowEvent,
        Unchosen,
    },
    session::{Delivered, Destination, Session},
    worker::{Completion, Worker},
};
use crate::{
    config::{Change, Config, Engine},
    show::SavedShow,
    sounds::Cue,
    transcript::{self, vocabulary::Vocabulary},
};

const INSERT_FAILED: &str =
    "Cannot submit the text. Check desktop permissions and clipboard access, then try again.";

enum Outcome {
    Idle,
    Done,
    Practiced,
    Copied(&'static str),
    ShowCopied {
        ending: Ending,
        preservation: Preservation,
    },
    ShowGone,
    Recovered {
        preservation: Preservation,
        narrated_copy: bool,
    },
    Empty,
    Cancelled,
    Unchosen(Unchosen),
    Failed(String),
}

impl Outcome {
    fn message(&self, notice: Option<&Notice>) -> String {
        let mut message = match self {
            Self::Practiced => "Practice complete. Your words stay in Settings.".into(),
            Self::Failed(message) => message.clone(),
            Self::Copied(message) => (*message).into(),
            Self::ShowCopied {
                ending,
                preservation,
            } => {
                let copied = match (ending, preservation.narration_lost.is_some()) {
                    (Ending::Finished, false) => SHOW_COPIED,
                    (Ending::ScreenStopped, false) => {
                        "Show copied, but the screen stopped recording early."
                    },
                    (Ending::WindowClosed, false) => "Show copied, but the recorded window closed.",
                    (Ending::WindowOffScreen, false) => {
                        "Show copied, but the recorded window went off screen."
                    },
                    (Ending::Finished, true) => "Show copied without narration.",
                    (Ending::ScreenStopped, true) => {
                        "Show copied without narration, and the screen stopped recording early."
                    },
                    (Ending::WindowClosed, true) => {
                        "Show copied without narration, and the recorded window closed."
                    },
                    (Ending::WindowOffScreen, true) => {
                        "Show copied without narration, and the recorded window went off screen."
                    },
                };
                let mut message = copied.to_owned();
                if preservation.narration_lost.is_some()
                    && let Some(failure) = notice.and_then(Notice::failure)
                {
                    message.push(' ');
                    message.push_str(failure);
                }
                message
            },
            Self::Unchosen(Unchosen::Window) => {
                "No window to record under the pointer, so the Show was cancelled.".into()
            },
            Self::Unchosen(Unchosen::Area) => {
                "No area was chosen, so the Show was cancelled.".into()
            },
            Self::ShowGone => SHOW_GONE.into(),
            Self::Recovered {
                narrated_copy: true,
                ..
            } => "Narrated copy saved. The original Show is unchanged.".into(),
            Self::Recovered { preservation, .. } => match preservation.narration_lost {
                Some(NarrationLoss::NotTranscribed | NarrationLoss::AudioStopped) => {
                    "Show recovered without narration. Find it in Shows."
                },
                Some(NarrationLoss::Incomplete) => {
                    "Show recovered with incomplete narration. Find it in Shows."
                },
                None => "Show recovered. Find it in Shows.",
            }
            .into(),
            Self::Idle | Self::Done | Self::Empty | Self::Cancelled => {
                notice.map_or("", Notice::text).into()
            },
        };
        if let Self::ShowCopied { preservation, .. } | Self::Recovered { preservation, .. } = self {
            if preservation.audio_kept {
                message.push_str(
                    " Audio is kept privately for retry in Shows until its original seven-day expiry.",
                );
            }
            if preservation.movie_lost {
                message
                    .push_str(" Video could not be saved; the selected keyframes are available.");
            }
            if preservation.source_lost {
                message.push_str(
                    " Retained source frames could not be saved; the selected keyframes are available.",
                );
            }
        }
        message
    }

    const fn phase(&self) -> Phase {
        match self {
            Self::Idle => Phase::Idle,
            Self::Done | Self::Practiced => Phase::Done,
            Self::Copied(_) | Self::ShowCopied { .. } => Phase::Copied,
            Self::ShowGone | Self::Recovered { .. } => Phase::Notice,
            Self::Empty => Phase::Empty,
            Self::Cancelled => Phase::Cancelled,
            Self::Unchosen(_) => Phase::Unchosen,
            Self::Failed(_) => Phase::Error,
        }
    }
}

enum Notice {
    CheckpointLost,
    MovieStopped,
    LoadingModel,
    SettingsApplied,
    WarmupFailed(String),
    NarrationLost(String),
}

impl Notice {
    fn text(&self) -> &str {
        match self {
            Self::CheckpointLost => {
                "Recovery stopped for this Show. Recording continues; finish it to save the full Show."
            },
            Self::MovieStopped => {
                "Video stopped for this Show. Still frames and narration continue; finish to save what was recorded."
            },
            Self::LoadingModel => LOADING,
            Self::SettingsApplied => "Settings applied",
            Self::WarmupFailed(failure) | Self::NarrationLost(failure) => failure,
        }
    }

    fn failure(&self) -> Option<&str> {
        match self {
            Self::WarmupFailed(failure) | Self::NarrationLost(failure) => Some(failure),
            Self::LoadingModel
            | Self::SettingsApplied
            | Self::CheckpointLost
            | Self::MovieStopped => None,
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
    Abandoned(SessionId, Result<Delivered, JoinError>),
    Retired,
    Recover(Recovery),
    Practice(mpsc::Sender<PracticeEvent>),
    EndPractice,
    Timer,
}

struct Latest {
    id: SessionId,
    started: Instant,
    show: bool,
}

struct Owner<P: Ports> {
    ports: P,
    config: Config,
    changes: watch::Receiver<Config>,
    inputs: Receiver<Input>,
    reports: Receiver<Report>,
    recoveries: Receiver<Recovery>,
    practices: Receiver<mpsc::Sender<PracticeEvent>>,
    practice: Option<mpsc::Sender<PracticeEvent>>,
    stopping: Receiver<Infallible>,
    capture_events: Sender<CaptureEvent>,
    captures: Receiver<CaptureEvent>,
    show_events: Sender<ShowEvent>,
    shows: Receiver<ShowEvent>,
    permits: InputSender,
    gesture: Gesture,
    session: Option<Session>,
    latest: Latest,
    last_show: Option<SavedShow>,
    reported: u64,
    worker: Worker<P::Speech>,
    microphone: Device<P::Microphone>,
    screen: Device<P::Screen>,
    retiring: Vec<(SessionId, JoinHandle<Delivered>)>,
    remembered: Option<SessionId>,
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
            recoveries,
            practices,
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
            recoveries,
            practices,
            practice: None,
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
            remembered: None,
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
        let idle = self.session.is_none();
        let practice_closed = or_pending(self.practice.as_ref().map(mpsc::Sender::closed));
        let delivered = or_pending(self.session.as_mut().map(Session::delivered));

        tokio::select! {
            biased;
            _ = self.stopping.recv() => Wake::Stop,
            () = practice_closed => Wake::EndPractice,
            Ok(practice) = self.practices.recv() => Wake::Practice(practice),
            input = self.inputs.recv() => Wake::Input(input),
            report = self.reports.recv() => report.map_or(Wake::Stop, Wake::Report),
            change = self.changes.changed() => change.map_or(Wake::Stop, |()| Wake::Configuration),
            Ok(event) = self.captures.recv() => Wake::Capture(event),
            Ok(event) = self.shows.recv() => Wake::Show(event),
            Ok(recovery) = self.recoveries.recv(), if idle => Wake::Recover(recovery),
            () = self.microphone.retired() => Wake::Retired,
            () = self.screen.retired() => Wake::Retired,
            completion = self.worker.completed() => Wake::Worker(completion),
            delivered = delivered => Wake::Delivered(delivered),
            (session, finished) = next_finished(&mut self.retiring) => {
                Wake::Abandoned(session, finished)
            },
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
            Wake::Recover(recovery) => return self.start_recovery(recovery),
            Wake::Practice(practice) => {
                self.arm_practice(practice);
                None
            },
            Wake::EndPractice => {
                self.end_practice();
                None
            },
            Wake::Worker(completion) => {
                self.handle_worker(completion);
                None
            },
            Wake::Delivered(result) => {
                self.handle_delivered(result);
                None
            },
            Wake::Abandoned(session, result) => {
                self.handle_abandoned(session, result);
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
            Input::StartShow(scope)
                if self.desktop_ready && self.gesture.state() == State::Idle =>
            {
                self.end_practice();

                let start = self.gesture.toggle(now());
                self.handle_action(start)?;
                self.start_show(scope);
                None
            },
            Input::Press | Input::Toggle | Input::StartShow(_) => None,
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
            Input::Interrupt => match self.gesture.interrupt() {
                Some(Action::Cancel) => {
                    self.discard();
                    None
                },
                finish => finish,
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

    fn report(&mut self, report: Report) {
        report.forget_gone(&mut self.last_show);

        if self.session.is_some() {
            return;
        }
        if report.presents_anew() {
            self.reported = self.reported.wrapping_add(1);
        }
        self.outcome = match report {
            Report::Failed(message) | Report::Recurring(message) => Outcome::Failed(message),
            Report::ShowCopied => Outcome::ShowCopied {
                ending: Ending::Finished,
                preservation: Preservation::default(),
            },
            Report::ShowGone(_) => Outcome::ShowGone,
        };
    }

    fn configure(&mut self) {
        self.practice = None;
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
        self.notice = Some(if self.worker.model() == ModelState::Ready {
            Notice::SettingsApplied
        } else {
            Notice::LoadingModel
        });
    }

    fn handle_capture(&mut self, event: CaptureEvent) {
        let gesture = self.gesture.state();
        let stream_ready = matches!(self.worker, Worker::Ready(_));
        let Some(session) = self.current_capture_mut(event.session()) else {
            return;
        };
        match event {
            CaptureEvent::Streaming(_, audio) => {
                if stream_ready {
                    session.stream(audio);
                }
            },
            CaptureEvent::Piece(_, number, audio) => session.piece(number, audio),
            CaptureEvent::CheckpointLost(_) => {
                self.checkpoint_lost();
            },
            CaptureEvent::Ready(_) => {
                session.audio_ready(gesture, now());

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
                    self.notice = Some(Notice::NarrationLost(error.to_string()));
                    self.conclude_capture();
                } else {
                    self.fail(error.to_string());
                }
            },
        }
    }

    fn current_capture_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.session
            .as_mut()
            .filter(|session| session.id == id && session.is_capturing())
    }

    fn conclude_capture(&mut self) {
        self.microphone.retire();
        self.screen.finish();
        if self.gesture.finish().is_some() {
            self.cue(Cue::Stop);
        }
    }

    fn handle_worker(&mut self, completion: Completion<P::Speech>) {
        match completion {
            Completion::Abandoned(ticket, worker) => {
                self.worker = Worker::Ready(worker);
                if let Some(session) = &mut self.session {
                    session.failed(ticket);
                }
            },
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
                    if !matches!(self.notice, Some(Notice::NarrationLost(_))) {
                        self.notice = Some(Notice::WarmupFailed(error.to_string()));
                    }
                } else if self.session.as_mut().is_some_and(Session::not_transcribed) {
                    self.notice = Some(Notice::WarmupFailed(error.to_string()));
                } else {
                    self.fail(error.to_string());
                }
            },
            Completion::Transcribed(ticket, Ok((worker, transcript))) => {
                self.worker = Worker::Ready(worker);
                if let Some(text) = self
                    .session
                    .as_mut()
                    .and_then(|session| session.heard(ticket, transcript))
                {
                    self.insert_transcript(text);
                }
            },
            Completion::Transcribed(ticket, Err(error)) => {
                let replaced = self.worker.revive(&self.ports, &self.config);
                let Some(session) = self
                    .session
                    .as_mut()
                    .filter(|session| session.accepts(ticket))
                else {
                    return;
                };
                if replaced && session.is_transcribing() {
                    self.notice = Some(Notice::LoadingModel);
                }

                if !session.failed(ticket) {
                    if session.not_transcribed() {
                        self.notice = Some(Notice::NarrationLost(error.to_string()));
                    } else {
                        self.fail(error.to_string());
                    }
                }
            },
        }
    }

    fn session_survives_warmup_failure(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| !session.awaits_worker())
    }

    fn insert_transcript(&mut self, text: String) {
        let text = Vocabulary::new(&self.config.vocabulary).text(&transcript::for_insertion(
            &text,
            self.config.fillers(&self.config.language),
        ));
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.is_recognizing())
        else {
            return;
        };
        if text.is_empty() {
            self.end_session(Outcome::Empty);
        } else if let Destination::Practice(receipt) = &session.destination {
            let delivered = !receipt.is_closed()
                && session.permit.commit()
                && receipt
                    .try_send(PracticeEvent::Text(zeroize::Zeroizing::new(text)))
                    .is_ok();
            self.end_session(if delivered {
                Outcome::Practiced
            } else {
                Outcome::Cancelled
            });
        } else {
            session.insert(|permit| self.ports.insert(text, permit, self.config.delivery()));
        }
    }

    fn start_show(&mut self, scope: Scope) {
        let held = matches!(self.gesture.state(), State::Held | State::HandsFree);
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if !held || !session.turn_into_show(scope) {
            return;
        }
        self.microphone.whole();
        let recorded = std::time::SystemTime::now()
            .checked_sub(now().saturating_duration_since(session.started))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let lost = match self.ports.keep(recorded) {
            Ok(Some(kept)) => {
                session.keep(kept);
                false
            },
            Ok(None) => false,
            Err(_) => true,
        };
        let recording = session.is_recording();
        if lost {
            self.checkpoint_lost();
        }
        self.latest.show = true;

        if recording {
            self.cue_heard(Cue::Show);
        }
        self.gesture.show();
    }

    fn handle_show(&mut self, event: ShowEvent) -> Option<Action> {
        let session = self
            .session
            .as_mut()
            .filter(|session| session.id == event.session() && session.is_show())?;
        let footage = match event {
            ShowEvent::CheckpointLost(_) => {
                self.checkpoint_lost();
                return None;
            },
            ShowEvent::MovieStopped(_) => {
                if self
                    .notice
                    .as_ref()
                    .is_none_or(|notice| notice.failure().is_none())
                {
                    self.notice = Some(Notice::MovieStopped);
                }
                return None;
            },
            ShowEvent::Finished(_, footage) => footage,
            ShowEvent::Unchosen(_, unchosen) => {
                session.forfeit();
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
                if !error.is::<crate::show::Unwound>() {
                    session.forfeit();
                }
                self.fail(format!("{error:#}"));
                None
            },
        }
    }

    fn handle_delivered(&mut self, result: Result<Delivered, JoinError>) {
        let show = self.session.as_ref().is_some_and(Session::is_show);
        let outcome = match result {
            Ok(Delivered::Reopened(Ok(reopened))) => {
                if let Some(session) = &mut self.session {
                    session.reopened(*reopened);
                }
                return;
            },
            Ok(Delivered::Inserted(Ok(Inserted::Sent))) => Outcome::Done,
            Ok(Delivered::Inserted(Ok(Inserted::Copied(message)))) => Outcome::Copied(message),
            Ok(Delivered::Published(Ok(Published::Saved {
                show: saved,
                ending,
                preservation,
                copied,
            }))) => {
                self.remember(saved, self.latest.id);
                match copied {
                    Ok(()) => Outcome::ShowCopied {
                        ending,
                        preservation,
                    },
                    Err(error) => {
                        let show = if ending == Ending::Finished {
                            "The Show"
                        } else {
                            "The Show, which stopped early,"
                        };
                        let (narration, failure) = match preservation.narration_lost {
                            Some(_) => (
                                " without narration",
                                self.notice.as_ref().and_then(Notice::failure),
                            ),
                            None => ("", None),
                        };
                        Outcome::Failed(format!(
                            "{show} was saved{narration} but not copied ({error:#}). Choose Copy \
                             last Show in Speakeasy's menu.{}",
                            failure.map_or_else(String::new, |failure| format!(" {failure}"))
                        ))
                    },
                }
            },
            Ok(
                Delivered::Inserted(Ok(Inserted::Cancelled))
                | Delivered::Published(Ok(Published::Cancelled))
                | Delivered::Abandoned(Ok(())),
            ) => Outcome::Cancelled,
            Ok(Delivered::Inserted(Ok(Inserted::Unavailable(message)))) => {
                Outcome::Failed(message.into())
            },
            Ok(Delivered::Published(Ok(Published::Recovered {
                show: saved,
                preservation,
                narrated_copy,
            }))) => {
                self.remember(saved, self.latest.id);
                Outcome::Recovered {
                    preservation,
                    narrated_copy,
                }
            },
            Ok(
                Delivered::Abandoned(Err(error))
                | Delivered::Published(Err(error))
                | Delivered::Reopened(Err(error)),
            ) => Outcome::Failed(format!("{error:#}")),
            Err(_) if show => Outcome::Failed(PUBLISH_STOPPED.into()),
            Ok(Delivered::Inserted(Err(_))) | Err(_) => Outcome::Failed(INSERT_FAILED.into()),
        };
        if let Outcome::Failed(message) = &outcome
            && let Some(session) = &mut self.session
        {
            session.recovery_failed(message);
        }
        if matches!(outcome, Outcome::Copied(_) | Outcome::ShowCopied { .. }) {
            self.cue(Cue::Copied);
        }

        self.end_session(outcome);
    }

    fn remember(&mut self, show: SavedShow, session: SessionId) {
        keep_presented(&self.snapshots, &show);
        self.last_show = Some(show);
        self.remembered = Some(session);
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
        let mut session = Session::new(id, permit, self.latest.started);
        if let Some(practice) = &self.practice {
            session.destination = Destination::Practice(practice.clone());
        }
        self.session = Some(session);
        ControlFlow::Continue(())
    }

    fn arm_practice(&mut self, receipt: mpsc::Sender<PracticeEvent>) {
        if self.session.is_some() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A closed practice view no longer wants its rejection"
            )]
            let _ = receipt.try_send(PracticeEvent::Unavailable(
                "Finish the current recording before trying dictation here.",
            ));
        } else if receipt.try_send(PracticeEvent::Ready).is_ok() {
            self.practice = Some(receipt);
        }
    }

    fn end_practice(&mut self) {
        self.practice = None;
        if self.session.as_ref().is_some_and(Session::is_practice) {
            self.gesture.abandon();
            self.discard();
            self.cue(Cue::Cancel);
        }
    }

    fn start_recovery(&mut self, recovery: Recovery) -> ControlFlow<()> {
        if !self.gesture.save() {
            return ControlFlow::Continue(());
        }
        let Some(permit) = self.permits.begin() else {
            self.gesture.abandon();
            return ControlFlow::Continue(());
        };
        let Some(id) = self.latest.id.next() else {
            return ControlFlow::Break(());
        };
        if !permit.shield() {
            self.gesture.abandon();
            return ControlFlow::Continue(());
        }
        self.latest = Latest {
            id,
            started: now(),
            show: true,
        };
        self.outcome = Outcome::Idle;
        self.notice = None;
        let work = self.ports.reopen(recovery.interrupted);
        self.session = Some(Session::recovering(
            id,
            permit,
            self.latest.started,
            recovery.completed,
            work,
        ));
        ControlFlow::Continue(())
    }

    fn hold_cues(&mut self) {
        let held = self.config.play_sounds && self.session.is_some();
        if held != self.cues_held {
            self.cues_held = held;
            self.ports.hold_cues(held);
        }
    }

    fn cue(&self, cue: Cue) {
        if self.config.play_sounds {
            self.ports.cue(cue);
        }
    }

    fn cue_heard(&self, cue: Cue) {
        if self.config.play_sounds {
            self.microphone.expect_cue();
            self.ports.cue(cue);
        }
    }

    fn end_session(&mut self, outcome: Outcome) {
        if let Some(session) = self.session.take() {
            session.permit.revoke();
        }
        self.gesture.complete();
        if matches!(
            self.notice,
            Some(Notice::CheckpointLost | Notice::MovieStopped)
        ) {
            self.notice = None;
        }
        self.outcome = outcome;
    }

    fn discard(&mut self) {
        self.abandon();
        self.worker.revive(&self.ports, &self.config);
        self.outcome = Outcome::Cancelled;
    }

    fn abandon(&mut self) {
        if matches!(
            self.notice,
            Some(Notice::CheckpointLost | Notice::MovieStopped)
        ) {
            self.notice = None;
        }
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
        if (session.is_transcribing() && self.worker.is_transcribing())
            || self.worker.is_streaming(session.id)
        {
            self.worker.recover(&self.ports, &self.config);
        }
        let id = session.id;
        if let Some(task) = session.into_cleanup() {
            self.retiring.push((id, task));
        }
    }

    fn fail(&mut self, message: String) {
        self.gesture.abandon();
        self.abandon();
        self.outcome = Outcome::Failed(message);
    }

    fn advance(&mut self) {
        self.open_queued_capture();
        self.attach_audio();
        self.open_queued_screen();
        self.start_transcription();
        self.start_publishing();
    }

    fn checkpoint_lost(&mut self) {
        if self
            .notice
            .as_ref()
            .is_none_or(|notice| notice.failure().is_none())
        {
            self.notice = Some(Notice::CheckpointLost);
        }
    }

    fn attach_audio(&mut self) {
        if let Some((log, start)) = self.session.as_mut().and_then(Session::audio)
            && !self.microphone.keep(log, start, &self.config.language)
        {
            self.checkpoint_lost();
        }
    }

    fn open_queued_capture(&mut self) {
        if self.permits.is_closed() || !self.microphone.is_free() {
            return;
        }
        let Some(session) = self.session.as_mut().filter(|session| session.is_queued()) else {
            return;
        };
        let microphone = self.config.microphone.as_deref();
        let recognition = if session.is_show() || self.config.engine == Engine::Whisper {
            super::ports::RecognitionMode::Whole
        } else if self.config.uses_nemotron() {
            super::ports::RecognitionMode::Streaming
        } else {
            super::ports::RecognitionMode::Segmented
        };
        match self.ports.record_microphone(
            session.id,
            microphone,
            recognition,
            self.capture_events.clone(),
        ) {
            Ok(recording) => {
                self.microphone.open(recording, self.gesture.state());
                session.opened();
            },
            Err(error) => self.fail(format!("Cannot open the microphone ({error}). Try again.")),
        }
    }

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
        let log = session.frames();
        match self.ports.record_screen(
            session.id,
            session.started,
            scope,
            log,
            self.show_events.clone(),
        ) {
            Ok(recorder) => {
                self.screen.open(recorder, self.gesture.state());
                session.screen_opened();
            },
            Err(error) => {
                session.forfeit();
                self.fail(format!("Cannot record the screen: {error}"));
            },
        }
    }

    fn start_transcription(&mut self) {
        if let Some(text) = self.session.as_mut().and_then(Session::complete) {
            self.insert_transcript(text);
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.awaits_worker() && self.worker.revive(&self.ports, &self.config) {
            self.notice = Some(Notice::LoadingModel);
        }
        self.worker.transcribe(&self.config, || session.request());
    }

    fn start_publishing(&mut self) {
        if let Some(session) = &mut self.session {
            session.publish(|publication, permit| self.ports.publish(publication, permit));
        }
    }

    fn handle_abandoned(&mut self, session: SessionId, result: Result<Delivered, JoinError>) {
        match result {
            Ok(Delivered::Abandoned(Err(error))) => {
                if self.session.is_none() && self.latest.id == session {
                    self.outcome =
                        Outcome::Failed(format!("Show recovery storage failed: {error:#}"));
                }
            },
            result => {
                if let Some(show) = saved_anyway(self.remembered, session, result) {
                    self.remember(show, session);
                }
            },
        }
    }

    fn present(&self) {
        debug_assert_eq!(self.gesture.state() == State::Idle, self.session.is_none());
        let session = self.session.as_ref();
        let (ending, narration_lost) = match self.outcome {
            Outcome::ShowCopied {
                ending,
                preservation,
            } => (ending, preservation.narration_lost),
            _ => (Ending::Finished, None),
        };
        let snapshot = Snapshot {
            epoch: self.epoch,
            session: self.latest.id.into(),
            reported: self.reported,
            phase: session.map_or_else(|| self.outcome.phase(), Session::phase),
            hands_free: self.gesture.state() == State::HandsFree,
            show: self.latest.show || matches!(self.outcome, Outcome::ShowCopied { .. }),
            ending,
            narration_lost,
            started: self.latest.started,
            last_show: self.last_show.clone(),
            message: self.outcome.message(self.notice.as_ref()),
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

        self.captures.close();
        self.shows.close();
        while self.shows.try_recv().is_ok() {}
        self.abandon();

        let retiring = mem::take(&mut self.retiring);
        let retired = async {
            let mut finished = Vec::with_capacity(retiring.len());
            for (session, task) in retiring {
                finished.push((session, task.await));
            }
            finished
        };
        let ((), (), (), finished) = tokio::join!(
            self.worker.stop(),
            self.microphone.stop(),
            self.screen.stop(),
            retired
        );

        let mut remembered = self.remembered;
        for (session, result) in finished {
            if let Some(show) = saved_anyway(remembered, session, result) {
                keep_presented(&self.snapshots, &show);
                remembered = Some(session);
            }
        }
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

fn saved_anyway(
    remembered: Option<SessionId>,
    session: SessionId,
    result: Result<Delivered, JoinError>,
) -> Option<SavedShow> {
    let Ok(Delivered::Published(Ok(
        Published::Saved { show, .. } | Published::Recovered { show, .. },
    ))) = result
    else {
        return None;
    };
    remembered
        .is_none_or(|later| later < session)
        .then_some(show)
}

fn keep_presented(snapshots: &watch::Sender<Snapshot>, show: &SavedShow) {
    snapshots.send_if_modified(|presented| {
        presented
            .last_show
            .replace(show.clone())
            .is_none_or(|kept| kept != *show)
    });
}

fn next_finished<T>(
    tasks: &mut Vec<(SessionId, JoinHandle<T>)>,
) -> impl Future<Output = (SessionId, Result<T, JoinError>)> + '_ {
    poll_fn(move |cx| {
        let mut finished = None;
        tasks.retain_mut(|(session, task)| {
            if finished.is_some() {
                return true;
            }
            match Pin::new(task).poll(cx) {
                Poll::Ready(result) => {
                    finished = Some((*session, result));
                    false
                },
                Poll::Pending => true,
            }
        });
        finished.map_or(Poll::Pending, Poll::Ready)
    })
}

async fn or_pending<F: Future>(future: Option<F>) -> F::Output {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(folder: &str) -> Delivered {
        Delivered::Published(Ok(Published::Saved {
            show: SavedShow::assume(folder.to_owned()),
            ending: Ending::Finished,
            preservation: Preservation::default(),
            copied: Ok(()),
        }))
    }

    #[test]
    fn saved_show_messages_report_retained_audio_and_lost_source_independently() {
        for (audio_kept, source_lost, suffix) in [
            (false, false, ""),
            (
                true,
                false,
                " Audio is kept privately for retry in Shows until its original seven-day expiry.",
            ),
            (
                false,
                true,
                " Retained source frames could not be saved; the selected keyframes are available.",
            ),
            (
                true,
                true,
                " Audio is kept privately for retry in Shows until its original seven-day expiry. \
                 Retained source frames could not be saved; the selected keyframes are available.",
            ),
        ] {
            for (outcome, prefix) in [
                (
                    Outcome::ShowCopied {
                        ending: Ending::Finished,
                        preservation: Preservation {
                            source_lost,
                            audio_kept,
                            ..Preservation::default()
                        },
                    },
                    SHOW_COPIED,
                ),
                (
                    Outcome::Recovered {
                        preservation: Preservation {
                            source_lost,
                            audio_kept,
                            ..Preservation::default()
                        },
                        narrated_copy: false,
                    },
                    "Show recovered. Find it in Shows.",
                ),
            ] {
                assert_eq!(outcome.message(None), format!("{prefix}{suffix}"));
            }
        }
    }

    #[test]
    fn a_show_saved_after_its_session_was_abandoned_never_replaces_a_later_sessions() {
        let first = SessionId::FIRST;
        let second = first.next().expect("a second session");
        assert_eq!(
            saved_anyway(None, first, Ok(saved("first"))),
            Some(SavedShow::assume("first".to_owned()))
        );
        assert_eq!(
            saved_anyway(Some(first), second, Ok(saved("second"))),
            Some(SavedShow::assume("second".to_owned()))
        );
        assert_eq!(
            saved_anyway(Some(second), first, Ok(saved("first"))),
            None,
            "An earlier session's Show replaced a later one"
        );
    }
}
