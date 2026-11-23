// Outside this crate's tests, machine values print without their payload, so dictated text
// never reaches a log or panic message. Tests derive the full `Debug` that explorer traces print.
#[cfg(not(test))]
macro_rules! redacted {
    ($($name:ident),+ $(,)?) => {
        $(impl<P: $crate::machine::Payload> ::std::fmt::Debug for $name<P> {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                formatter.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        })+
    };
}

#[cfg(test)]
mod fixture;
mod outcome;
mod recognition;
mod session;

use std::{mem, ops::ControlFlow};

use self::session::{Purpose, Session};
pub use self::{
    outcome::{Failure, Notice, Outcome, Refusal},
    recognition::{Detail, Part, Partition, Request, Ticket, Transcript},
    session::{Enter, Handoff, Kept, Origin, Publication, Recognized, Release, Reopened, Sealed},
};
use crate::gesture::{Action, Gesture, Moment, State, Timing};

// Tests need every payload type `Clone + Debug` so the explorer can branch and print states;
// other builds add no bounds.
#[cfg(not(test))]
pub trait Cargo {}

#[cfg(not(test))]
impl<T> Cargo for T {}

#[cfg(test)]
pub trait Cargo: Clone + std::fmt::Debug {}

#[cfg(test)]
impl<T: Clone + std::fmt::Debug> Cargo for T {}

/// The types the machine carries without inspecting them.
///
/// The app's `runtime::native::Native` supplies real audio, text, and handles; the explorer and
/// fixtures supply small stand-ins, so any ordering of events runs without hardware.
pub trait Payload {
    type Config: Cargo;
    type Text: Cargo + Clone;
    type Style: Cargo + Copy + Default;
    type Permit: Cargo;
    type Instant: Cargo + Copy;
    type Span: Cargo + Copy;
    type Audio: Cargo + Clone;
    type Range: Cargo;
    type Live: Cargo;
    type Heard: Cargo + Clone + Default;
    type Narration: Cargo + Default;
    type Footage: Cargo;
    type Checkpoint: Cargo;
    type AudioLog: Cargo;
    type FrameLog: Cargo;
    type Scope: Cargo + Copy;
    type Pending: Cargo;
    type Interrupted: Cargo;
    type Receipt: Cargo;
    type Practice: Cargo + Clone;
    type Taps: Cargo;
    type Level: Cargo;
    type Shown: Cargo;
    type Saved: Cargo + Default;
    type Recovered: Cargo;
    type Unchosen: Cargo;
    type Call: Cargo;

    fn settings(config: &Self::Config) -> Settings;

    fn reloads(current: &Self::Config, next: &Self::Config) -> bool;

    fn offset(started: Self::Instant, at: Self::Instant) -> Self::Span;

    fn shift(span: Self::Span, by: Self::Span) -> Self::Span;

    fn delay(narration: Self::Narration, by: Self::Span) -> Self::Narration;

    fn append(text: &mut Self::Heard, next: Self::Heard);
}

/// Every side effect the machine asks for.
///
/// The app's `runtime::owner::World` performs them, the explorer's `Oracle` records them, and
/// `fixture::Log` does for unit tests. Calls return at once; their results come back later as
/// [`Event`]s.
pub trait Env<P: Payload> {
    /// Opens insertion authority for a new session; `None` once input has closed.
    fn admit(&mut self) -> Option<P::Permit>;

    fn style(&mut self) -> P::Style;

    /// Exempts the permit from native Escape, cancel, and interrupt, so only the machine ends it,
    /// as Shows and recoveries need. False when the permit is no longer current.
    fn shield(&mut self, permit: &P::Permit) -> bool;

    /// Ends the permit's authority, shielded or not.
    fn revoke(&mut self, permit: &P::Permit);

    /// Opens the crash checkpoint for a Show begun at `started`. A `Lost` checkpoint warns and
    /// the Show goes on without one.
    fn keep(&mut self, started: P::Instant) -> Keeping<P>;

    fn record_microphone(
        &mut self,
        session: SessionId,
        config: &P::Config,
        mode: RecognitionMode,
    ) -> Result<(), P::Text>;

    fn record_screen(
        &mut self,
        session: SessionId,
        started: P::Instant,
        scope: P::Scope,
        frames: Option<P::FrameLog>,
    ) -> Result<(), P::Text>;

    /// Asks the open recorder to stop and seal what it captured.
    fn finish(&mut self, recorder: Recorder);

    /// Tears the recorder down; [`Event::Retired`] reports when the device is free again.
    fn retire(&mut self, recorder: Recorder);

    /// Switches the open microphone to whole-recording recognition once a dictation becomes a
    /// Show.
    fn listen_whole(&mut self);

    /// Warns the open microphone that a cue sound is about to play, so speech detection does not
    /// count the cue as speech.
    fn expect_cue(&mut self);

    /// Starts saving microphone audio from `start` into the Show's checkpoint; false if refused.
    fn keep_audio(&mut self, log: P::AudioLog, start: P::Span, config: &P::Config) -> bool;

    fn pin(&mut self, taps: P::Taps);

    fn meter(&mut self, session: SessionId, level: P::Level);

    /// Starts the speech engine; `reload` restarts it for changed settings.
    fn load(&mut self, config: &P::Config);

    fn reload(&mut self, config: &P::Config);

    /// Restarts speech after a failure. The running engine is kept only if it answered or, with
    /// `probe`, it responds to a probe; otherwise a fresh one loads.
    fn recover(&mut self, config: &P::Config, probe: bool);

    /// Extracts `range` of `audio` for one segment's transcription request.
    fn cut(&mut self, audio: &P::Audio, range: &P::Range) -> Option<P::Audio>;

    fn transcribe(&mut self, request: Request<P>, config: &P::Config);

    /// Applies style, writing settings, and vocabulary to heard text; `None` when nothing is left.
    fn compose(&mut self, heard: P::Heard, style: P::Style, config: &P::Config)
    -> Option<P::Heard>;

    /// Hands final text over as `handoff` says: paste it into the focused app, print it to a
    /// waiting script, or report it unclaimed by the script that captured it. The outcome
    /// returns as [`Event::Delivered`].
    fn insert(
        &mut self,
        text: P::Heard,
        permit: &P::Permit,
        config: &P::Config,
        handoff: Handoff<P>,
    );

    /// Arms practice, the Settings try-it field that receives dictation instead of the focused
    /// app; false if its view has gone.
    fn ready(&mut self, practice: &P::Practice) -> bool;

    fn refuse_practice(&mut self, practice: P::Practice);

    fn practise(&mut self, practice: &P::Practice, permit: &P::Permit, text: P::Heard) -> bool;

    /// Saves a live or recovered Show; `receipt` answers the library's recovery request.
    fn publish(
        &mut self,
        publication: Publication<P>,
        permit: &P::Permit,
        receipt: Option<P::Receipt>,
    );

    fn reopen(&mut self, interrupted: P::Interrupted);

    fn refuse_recovery(&mut self, receipt: P::Receipt, failure: &Failure<P>);

    /// Moves an in-flight delivery aside so a newer session can start; it settles as
    /// [`Event::Abandoned`].
    fn retire_delivery(&mut self, session: SessionId);

    /// Releases an abandoned Show's checkpoint as `release` directs; settles as
    /// [`Event::Abandoned`].
    fn release(&mut self, session: SessionId, checkpoint: P::Checkpoint, release: Release);

    fn remember(&mut self, shown: P::Shown);

    /// Marks a new report so the UI shows feedback again even when the phase is unchanged.
    fn announce(&mut self);

    fn cue(&mut self, cue: Cue);

    /// Keeps the sound output open through a session so cues start without opening a device.
    fn hold_cues(&mut self, held: bool);

    fn respond(&mut self, call: P::Call, reply: Result<Status<P>, Refusal>);

    /// Closes the input, capture, and Show lanes at shutdown so no new work arrives.
    fn close_input(&mut self);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(u64);

impl SessionId {
    pub const FIRST: Self = Self(1);

    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl From<SessionId> for u64 {
    fn from(id: SessionId) -> Self {
        id.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Idle,
    Starting,
    Recording,
    Stopping,
    Processing,
    Done,
    Copied,
    Notice,
    Empty,
    Cancelled,
    Unchosen,
    Error,
}

impl Phase {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Recording => "recording",
            Self::Stopping => "stopping",
            Self::Processing => "processing",
            Self::Done => "done",
            Self::Copied => "copied",
            Self::Notice => "notice",
            Self::Empty => "empty",
            Self::Cancelled => "cancelled",
            Self::Unchosen => "unchosen",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Recording | Self::Stopping | Self::Processing
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelState {
    Loading,
    Ready,
    Unavailable,
}

impl ModelState {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NarrationLoss {
    Incomplete,
    AudioStopped,
    AudioSilent,
    NotTranscribed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cue {
    Start,
    Stop,
    Cancel,
    Show,
    Copied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecognitionMode {
    Whole,
    Segmented,
    Streaming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Recorder {
    Microphone,
    Screen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Settings {
    pub play_sounds: bool,
    pub recognition: RecognitionMode,
    pub probe: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Halt {
    Failed,
    Stopped,
}

#[cfg_attr(test, derive(Debug))]
pub struct Now<P: Payload> {
    pub moment: Moment,
    pub instant: P::Instant,
}

impl<P: Payload> Clone for Now<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Payload> Copy for Now<P> {}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Status<P: Payload> {
    pub phase: Phase,
    pub model: ModelState,
    pub desktop_ready: bool,
    pub session: Option<SessionId>,
    pub preset: Option<P::Text>,
    pub last_dictation: bool,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Keeping<P: Payload> {
    Kept(Kept<P>),
    Unneeded,
    Lost,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Event<P: Payload> {
    Stop,
    InputLost,
    Input(Input<P>),
    Report(Report<P>),
    Configure(P::Config),
    Revert(P::Config),
    Control {
        call: P::Call,
        command: Command<P>,
    },
    Preset {
        call: P::Call,
        name: P::Text,
        config: P::Config,
    },
    Capture(SessionId, Capture<P>),
    Show(SessionId, Screening<P>),
    Worker(Completion<P>),
    Delivered(Delivered<P>),
    Abandoned(SessionId, Delivered<P>),
    Retired(Recorder),
    Recover {
        interrupted: P::Interrupted,
        receipt: P::Receipt,
    },
    Practice(P::Practice),
    PracticeClosed,
    Timer,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Input<P: Payload> {
    Press,
    Release,
    Lock,
    Show(P::Scope),
    Pin(P::Taps),
    Escape,
    Cancel,
    Interrupt,
    Toggle,
    PasteLast,
    Submit,
    StartShow(P::Scope),
    Unavailable(P::Text),
    DesktopReady,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Report<P: Payload> {
    Failed(P::Text),
    Recurring(P::Text),
    ShowCopied,
    ShowGone,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Command<P: Payload> {
    Status,
    Start(Option<P::Style>),
    Capture(Option<P::Style>),
    Stop(Option<SessionId>),
    Print(Option<SessionId>),
    Cancel(Option<SessionId>),
    PasteLast,
    Unsupported,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Capture<P: Payload> {
    Streaming(P::Live),
    Ready,
    CheckpointLost,
    Level(P::Level),
    Piece(u32, P::Audio),
    Finished(Result<Option<Sealed<P>>, Unheard<P>>),
    Lost(Sealed<P>, Lost<P>),
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Unheard<P: Payload> {
    pub silent: bool,
    pub text: P::Text,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Lost<P: Payload> {
    pub notice: P::Text,
    pub failure: P::Text,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Screening<P: Payload> {
    CheckpointLost,
    MovieStopped,
    Finished(Result<P::Footage, Stopped<P>>),
    Unchosen(P::Unchosen),
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Stopped<P: Payload> {
    pub unwound: bool,
    pub text: P::Text,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Completion<P: Payload> {
    Abandoned(Ticket),
    Loaded(Result<(), P::Text>),
    Transcribed(Ticket, Result<Transcript<P>, P::Text>),
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Delivered<P: Payload> {
    Inserted(Inserted<P>),
    Published(Published<P>),
    Abandoned(Result<(), P::Text>),
    Reopened(Result<Reopened<P>, P::Text>),
    Panicked,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Inserted<P: Payload> {
    Sent,
    Copied(P::Text),
    Unavailable(P::Text),
    Cancelled,
    Failed,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Published<P: Payload> {
    Saved {
        shown: P::Shown,
        saved: P::Saved,
        copied: Result<(), P::Text>,
    },
    Recovered {
        shown: P::Shown,
        recovered: P::Recovered,
    },
    Cancelled,
    Failed(P::Text),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Worker {
    Unavailable,
    Loading,
    Ready,
    Transcribing(Ticket),
}

impl Worker {
    const fn model(self) -> ModelState {
        match self {
            Self::Unavailable => ModelState::Unavailable,
            Self::Loading => ModelState::Loading,
            Self::Ready | Self::Transcribing(_) => ModelState::Ready,
        }
    }

    fn is_streaming(self, session: SessionId) -> bool {
        self == Self::Transcribing(Ticket {
            session,
            part: Part::Stream,
        })
    }

    fn revive<P: Payload>(&mut self, config: &P::Config, env: &mut impl Env<P>) -> bool {
        let unavailable = *self == Self::Unavailable;
        if unavailable {
            env.load(config);
            *self = Self::Loading;
        }
        unavailable
    }

    fn recover<P: Payload>(&mut self, config: &P::Config, probe: bool, env: &mut impl Env<P>) {
        let streaming = matches!(
            self,
            Self::Transcribing(Ticket {
                part: Part::Stream,
                ..
            })
        );
        env.recover(config, probe || streaming);
        *self = Self::Loading;
    }

    fn transcribe<P: Payload>(
        &mut self,
        session: &mut Session<P>,
        config: &P::Config,
        env: &mut impl Env<P>,
    ) {
        if *self != Self::Ready {
            return;
        }
        if let Some(request) = session.request(env) {
            *self = Self::Transcribing(request.ticket());
            env.transcribe(request, config);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Device {
    Free,
    Open,
    Retiring,
}

#[cfg_attr(test, derive(Clone, Debug))]
struct Latest<P: Payload> {
    id: SessionId,
    started: P::Instant,
    show: bool,
    style: P::Style,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Machine<P: Payload> {
    config: P::Config,
    gesture: Gesture,
    desktop_ready: bool,
    latest: Latest<P>,
    session: Option<Session<P>>,
    practice: Option<P::Practice>,
    preset: Option<P::Text>,
    last_dictation: Option<P::Heard>,
    remembered: Option<SessionId>,
    worker: Worker,
    microphone: Device,
    screen: Device,
    retiring: Vec<SessionId>,
    cues_held: bool,
    outcome: Outcome<P>,
    notice: Option<Notice<P>>,
}

impl<P: Payload> Machine<P> {
    #[must_use]
    pub fn new(
        timing: Timing,
        config: P::Config,
        desktop_ready: bool,
        started: P::Instant,
        preset: Option<P::Text>,
        env: &mut impl Env<P>,
    ) -> Self {
        env.load(&config);
        Self {
            config,
            gesture: Gesture::new(timing),
            desktop_ready,
            latest: Latest {
                id: SessionId::default(),
                started,
                show: false,
                style: P::Style::default(),
            },
            session: None,
            practice: None,
            preset,
            last_dictation: None,
            remembered: None,
            worker: Worker::Loading,
            microphone: Device::Free,
            screen: Device::Free,
            retiring: Vec::new(),
            cues_held: false,
            outcome: Outcome::Idle,
            notice: Some(Notice::LoadingModel),
        }
    }

    pub fn step(
        &mut self,
        now: Now<P>,
        event: Event<P>,
        env: &mut impl Env<P>,
    ) -> ControlFlow<Halt> {
        self.handle(now, event, env)?;
        self.advance(env);
        self.hold_cues(env);
        ControlFlow::Continue(())
    }

    pub fn shut_down(&mut self, env: &mut impl Env<P>) {
        env.close_input();
        self.abandon(env);
    }

    #[must_use]
    pub fn drained(&mut self, session: SessionId, delivered: Delivered<P>) -> Option<P::Shown> {
        let shown = saved_anyway(self.remembered, session, delivered)?;
        self.remembered = Some(session);
        Some(shown)
    }

    #[must_use]
    pub fn deadline(&self) -> Option<Moment> {
        self.gesture.deadline()
    }

    #[must_use]
    pub const fn is_idle(&self) -> bool {
        self.session.is_none()
    }

    #[must_use]
    pub const fn practice(&self) -> Option<&P::Practice> {
        self.practice.as_ref()
    }

    #[must_use]
    pub const fn last_dictation(&self) -> Option<&P::Heard> {
        self.last_dictation.as_ref()
    }

    #[must_use]
    pub const fn latest_session(&self) -> SessionId {
        self.latest.id
    }

    #[must_use]
    pub const fn started(&self) -> P::Instant {
        self.latest.started
    }

    #[must_use]
    pub const fn style(&self) -> P::Style {
        self.latest.style
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        self.session
            .as_ref()
            .map_or_else(|| self.outcome.phase(), Session::phase)
    }

    #[must_use]
    pub fn hands_free(&self) -> bool {
        self.gesture.state() == State::HandsFree
    }

    #[must_use]
    pub fn for_script(&self) -> bool {
        self.session.as_ref().is_some_and(Session::for_script)
    }

    #[must_use]
    pub const fn shows(&self) -> bool {
        self.latest.show || matches!(self.outcome, Outcome::ShowCopied(_))
    }

    #[must_use]
    pub const fn outcome(&self) -> &Outcome<P> {
        &self.outcome
    }

    #[must_use]
    pub const fn notice(&self) -> Option<&Notice<P>> {
        self.notice.as_ref()
    }

    #[must_use]
    pub const fn preset(&self) -> Option<&P::Text> {
        self.preset.as_ref()
    }

    #[must_use]
    pub const fn model(&self) -> ModelState {
        self.worker.model()
    }

    #[must_use]
    pub const fn desktop_ready(&self) -> bool {
        self.desktop_ready
    }

    fn status(&self) -> Status<P> {
        Status {
            phase: self.phase(),
            model: self.worker.model(),
            desktop_ready: self.desktop_ready,
            session: self.control_session(),
            preset: self.preset.clone(),
            last_dictation: self.last_dictation.is_some(),
        }
    }

    fn handle(&mut self, now: Now<P>, event: Event<P>, env: &mut impl Env<P>) -> ControlFlow<Halt> {
        let action = match event {
            Event::Stop => return ControlFlow::Break(Halt::Stopped),
            Event::InputLost => return self.stop_with_failure(Failure::InputLost, env),
            Event::Input(input) => self.handle_input(now, input, env)?,
            Event::Report(report) => {
                self.report(report, env);
                None
            },
            Event::Configure(config) => {
                self.configure(config, Outcome::Idle, env);
                None
            },
            Event::Revert(config) => {
                let outcome = mem::replace(&mut self.outcome, Outcome::Idle);
                let lost = self
                    .notice
                    .take_if(|notice| matches!(notice, Notice::NarrationLost(_)));
                self.configure(config, outcome, env);
                if lost.is_some() {
                    self.notice = lost;
                }
                None
            },
            Event::Control { call, command } => return self.control(now, call, command, env),
            Event::Preset { call, name, config } => {
                let adopted = self.adopt_preset(name, config, env);
                env.respond(call, adopted.map(|()| self.status()));
                None
            },
            Event::Capture(session, capture) => {
                self.handle_capture(now, session, capture, env);
                None
            },
            Event::Show(session, screening) => self.handle_show(session, screening, env),
            Event::Worker(completion) => {
                self.handle_worker(completion, env);
                None
            },
            Event::Delivered(delivered) => {
                self.handle_delivered(delivered, env);
                None
            },
            Event::Abandoned(session, delivered) => {
                self.handle_abandoned(session, delivered, env);
                None
            },
            Event::Retired(recorder) => {
                let device = self.device(recorder);
                if *device == Device::Retiring {
                    *device = Device::Free;
                }
                None
            },
            Event::Recover {
                interrupted,
                receipt,
            } => return self.start_recovery(now, interrupted, receipt, env),
            Event::Practice(practice) => {
                self.arm_practice(practice, env);
                None
            },
            Event::PracticeClosed => {
                self.end_practice(env);
                None
            },
            Event::Timer => self.gesture.tick(now.moment),
        };
        self.handle_action(now, action, env)
    }

    fn handle_input(
        &mut self,
        now: Now<P>,
        input: Input<P>,
        env: &mut impl Env<P>,
    ) -> ControlFlow<Halt, Option<Action>> {
        let ready = self.desktop_ready;
        let action = match input {
            Input::Press if ready => self.gesture.press(now.moment),
            Input::Toggle if ready => self.gesture.toggle(now.moment),
            Input::PasteLast if ready => {
                self.paste_last(now, env)?;
                None
            },
            Input::StartShow(scope) if ready && self.gesture.state() == State::Idle => {
                self.end_practice(env);
                let start = self.gesture.toggle(now.moment);
                self.handle_action(now, start, env)?;
                if self.session.is_some() && !self.start_show(scope, env) {
                    self.discard(env);
                }
                None
            },
            Input::Press | Input::Toggle | Input::PasteLast | Input::StartShow(_) => None,
            Input::Release => self.gesture.release(now.moment),
            Input::Lock => {
                self.gesture.lock();
                None
            },
            Input::Submit => {
                let action = self.gesture.submit();
                if action.is_some()
                    && let Some(session) = &mut self.session
                {
                    session.submit();
                }
                action
            },
            Input::Show(scope) => {
                self.start_show(scope, env);
                None
            },
            Input::Pin(taps) => {
                if self.screen == Device::Open {
                    env.pin(taps);
                }
                None
            },
            Input::Escape => self.gesture.escape(),
            Input::Cancel => self.gesture.cancel(),
            Input::Interrupt => match self.gesture.interrupt() {
                Some(Action::Cancel) => {
                    self.discard(env);
                    None
                },
                finish => finish,
            },
            Input::Unavailable(text) => return self.stop_with_failure(Failure::Said(text), env),
            Input::DesktopReady => {
                self.desktop_ready = true;
                None
            },
        };
        ControlFlow::Continue(action)
    }

    fn stop_with_failure<T>(
        &mut self,
        failure: Failure<P>,
        env: &mut impl Env<P>,
    ) -> ControlFlow<Halt, T> {
        self.fail(failure, env);
        ControlFlow::Break(Halt::Failed)
    }

    fn report(&mut self, report: Report<P>, env: &mut impl Env<P>) {
        if self.session.is_some() {
            return;
        }
        if !matches!(report, Report::Recurring(_)) {
            env.announce();
        }
        self.outcome = match report {
            Report::Failed(text) | Report::Recurring(text) => Outcome::Failed(Failure::Said(text)),
            Report::ShowCopied => {
                self.cue(Cue::Copied, env);
                Outcome::ShowCopied(P::Saved::default())
            },
            Report::ShowGone => Outcome::ShowGone,
        };
    }

    fn configure(&mut self, config: P::Config, settled: Outcome<P>, env: &mut impl Env<P>) {
        self.practice = None;
        self.preset = None;
        let cancelled = self.gesture.abandon().is_some();
        self.apply_settings(config, env);
        self.outcome = if cancelled {
            self.cue(Cue::Cancel, env);
            Outcome::Cancelled
        } else {
            settled
        };
    }

    fn apply_settings(&mut self, config: P::Config, env: &mut impl Env<P>) {
        let reload = P::reloads(&self.config, &config);
        self.config = config;
        if reload {
            env.reload(&self.config);
            self.worker = Worker::Loading;
        }
        self.abandon(env);
        self.worker.revive(&self.config, env);
        self.notice = Some(if self.worker.model() == ModelState::Ready {
            Notice::SettingsApplied
        } else {
            Notice::LoadingModel
        });
    }

    fn handle_capture(
        &mut self,
        now: Now<P>,
        id: SessionId,
        capture: Capture<P>,
        env: &mut impl Env<P>,
    ) {
        let gesture = self.gesture.state();
        let stream_ready = self.worker == Worker::Ready;
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.id == id && session.is_capturing())
        else {
            return;
        };
        match capture {
            Capture::Streaming(audio) => {
                if stream_ready {
                    session.stream(audio);
                }
            },
            Capture::Piece(number, audio) => session.piece(number, audio),
            Capture::CheckpointLost => self.warn(Notice::CheckpointLost),
            Capture::Ready => {
                session.audio_ready(gesture, now.instant);
                let cue = if session.is_show() {
                    Cue::Show
                } else {
                    Cue::Start
                };
                if session.is_recording() {
                    self.cue_heard(cue, env);
                }
            },
            Capture::Level(level) => env.meter(id, level),
            Capture::Finished(Ok(sealed)) => {
                let delivers = session.captured(sealed);
                self.conclude_capture(env);
                if !delivers {
                    self.end_session(Outcome::Empty, env);
                }
            },
            Capture::Finished(Err(unheard)) => {
                if session.go_on_without_audio(unheard.silent) {
                    self.notice = Some(Notice::NarrationLost(unheard.text));
                    self.conclude_capture(env);
                } else {
                    self.fail(Failure::Said(unheard.text), env);
                }
            },
            Capture::Lost(sealed, lost) if session.is_listening() && !session.is_practice() => {
                if session.is_show() {
                    self.notice = Some(Notice::NarrationLost(lost.notice.clone()));
                }
                session.lost_microphone(sealed, lost);
                self.conclude_capture(env);
            },
            Capture::Lost(_, lost) => self.fail(Failure::Said(lost.failure), env),
        }
    }

    fn conclude_capture(&mut self, env: &mut impl Env<P>) {
        self.retire(Recorder::Microphone, env);
        self.finish(Recorder::Screen, env);
        if self.gesture.finish().is_some() {
            self.cue(Cue::Stop, env);
        }
    }

    fn handle_worker(&mut self, completion: Completion<P>, env: &mut impl Env<P>) {
        self.worker = Worker::Unavailable;
        match completion {
            Completion::Abandoned(ticket) => {
                self.worker = Worker::Ready;
                if let Some(session) = &mut self.session {
                    session.absorb_failure(ticket);
                }
            },
            Completion::Loaded(Ok(())) => {
                self.worker = Worker::Ready;
                if matches!(
                    self.notice,
                    Some(Notice::LoadingModel | Notice::WarmupFailed(_))
                ) {
                    self.notice = None;
                }
            },
            Completion::Loaded(Err(text)) => {
                if self.session_survives_warmup_failure() {
                    if !matches!(self.notice, Some(Notice::NarrationLost(_))) {
                        self.notice = Some(Notice::WarmupFailed(text));
                    }
                } else if let Some(session) = &mut self.session
                    && session.go_on_without_transcript()
                {
                    self.notice = Some(Notice::WarmupFailed(text));
                } else {
                    self.fail(Failure::Said(text), env);
                }
            },
            Completion::Transcribed(ticket, Ok(transcript)) => {
                self.worker = Worker::Ready;
                if let Some(text) = self
                    .session
                    .as_mut()
                    .and_then(|session| session.heard(ticket, transcript))
                {
                    self.insert_transcript(text, env);
                }
            },
            Completion::Transcribed(ticket, Err(text)) => {
                self.worker.revive(&self.config, env);
                let Some(session) = self
                    .session
                    .as_mut()
                    .filter(|session| session.accepts(ticket))
                else {
                    return;
                };
                if session.is_transcribing()
                    && !matches!(self.notice, Some(Notice::NarrationLost(_)))
                {
                    self.notice = Some(Notice::LoadingModel);
                }
                if !session.absorb_failure(ticket) {
                    if session.go_on_without_transcript() {
                        self.notice = Some(Notice::NarrationLost(text));
                    } else {
                        self.fail(Failure::Said(text), env);
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

    fn insert_transcript(&mut self, heard: P::Heard, env: &mut impl Env<P>) {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.is_recognizing())
        else {
            return;
        };
        let lost = session.take_lost();
        let Some(text) = env.compose(heard, session.style, &self.config) else {
            let outcome = lost.map_or(Outcome::Empty, |lost| {
                Outcome::Failed(Failure::Said(lost.failure))
            });
            self.end_session(outcome, env);
            return;
        };
        if let Some(practice) = session.practice() {
            let delivered = env.practise(practice, &session.permit, text);
            self.end_session(
                if delivered {
                    Outcome::Practiced
                } else {
                    Outcome::Cancelled
                },
                env,
            );
        } else if let Some(lost) = lost {
            self.last_dictation = Some(text);
            self.end_session(Outcome::Withheld(lost.notice), env);
        } else {
            session.insert(text, &self.config, env);
        }
    }

    fn start_show(&mut self, scope: P::Scope, env: &mut impl Env<P>) -> bool {
        let held = matches!(self.gesture.state(), State::Held | State::HandsFree);
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        if !held || !session.turn_into_show(scope, env) {
            return false;
        }
        if self.microphone == Device::Open {
            env.listen_whole();
        }
        let recording = session.is_recording();
        match env.keep(session.started) {
            Keeping::Kept(kept) => session.keep(kept),
            Keeping::Unneeded => {},
            Keeping::Lost => self.warn(Notice::CheckpointLost),
        }
        self.latest.show = true;
        if recording {
            self.cue_heard(Cue::Show, env);
        }
        self.gesture.show();
        true
    }

    fn handle_show(
        &mut self,
        id: SessionId,
        screening: Screening<P>,
        env: &mut impl Env<P>,
    ) -> Option<Action> {
        let session = self
            .session
            .as_mut()
            .filter(|session| session.id == id && session.is_show())?;
        match screening {
            Screening::CheckpointLost => {
                self.warn(Notice::CheckpointLost);
                None
            },
            Screening::MovieStopped => {
                self.warn(Notice::MovieStopped);
                None
            },
            Screening::Finished(Ok(footage)) => {
                session.screen_recorded(footage);
                self.retire(Recorder::Screen, env);
                self.gesture.finish()
            },
            Screening::Finished(Err(stopped)) => {
                if !stopped.unwound {
                    session.forfeit();
                }
                self.fail(Failure::Said(stopped.text), env);
                None
            },
            Screening::Unchosen(unchosen) => {
                session.forfeit();
                self.discard(env);
                self.cue(Cue::Cancel, env);
                self.outcome = Outcome::Unchosen(unchosen);
                None
            },
        }
    }

    fn handle_delivered(&mut self, delivered: Delivered<P>, env: &mut impl Env<P>) {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.awaits_delivery())
        else {
            return;
        };
        let show = session.is_show();
        let keeps_dictation = matches!(
            delivered,
            Delivered::Inserted(
                Inserted::Sent | Inserted::Copied(_) | Inserted::Unavailable(_) | Inserted::Failed
            ) | Delivered::Panicked
        );
        if keeps_dictation && let Some(text) = session.take_dictation() {
            self.last_dictation = Some(text);
        }
        let outcome = match delivered {
            Delivered::Reopened(Ok(reopened)) => {
                session.reopened(reopened);
                return;
            },
            Delivered::Inserted(Inserted::Sent) => Outcome::Done,
            Delivered::Inserted(Inserted::Copied(text)) => Outcome::Copied(text),
            Delivered::Published(Published::Saved {
                shown,
                saved,
                copied,
            }) => {
                self.remember(shown, self.latest.id, env);
                match copied {
                    Ok(()) => Outcome::ShowCopied(saved),
                    Err(error) => Outcome::Failed(Failure::NotCopied {
                        saved,
                        error,
                        failure: self.notice.as_ref().and_then(Notice::failure).cloned(),
                    }),
                }
            },
            Delivered::Inserted(Inserted::Cancelled)
            | Delivered::Published(Published::Cancelled)
            | Delivered::Abandoned(Ok(())) => Outcome::Cancelled,
            Delivered::Inserted(Inserted::Unavailable(text)) => {
                Outcome::Failed(Failure::Undelivered(Some(text)))
            },
            Delivered::Published(Published::Recovered { shown, recovered }) => {
                self.remember(shown, self.latest.id, env);
                Outcome::Recovered(recovered)
            },
            Delivered::Abandoned(Err(text))
            | Delivered::Published(Published::Failed(text))
            | Delivered::Reopened(Err(text)) => Outcome::Failed(Failure::Said(text)),
            Delivered::Panicked if show => Outcome::Failed(Failure::PublishStopped),
            Delivered::Inserted(Inserted::Failed) | Delivered::Panicked => {
                Outcome::Failed(Failure::Undelivered(None))
            },
        };
        if let Outcome::Failed(failure) = &outcome
            && let Some(receipt) = self.session.as_mut().and_then(Session::take_receipt)
        {
            env.refuse_recovery(receipt, failure);
        }
        if matches!(outcome, Outcome::Copied(_) | Outcome::ShowCopied(_)) {
            self.cue(Cue::Copied, env);
        }
        self.end_session(outcome, env);
    }

    fn remember(&mut self, shown: P::Shown, session: SessionId, env: &mut impl Env<P>) {
        env.remember(shown);
        self.remembered = Some(session);
    }

    fn handle_action(
        &mut self,
        now: Now<P>,
        action: Option<Action>,
        env: &mut impl Env<P>,
    ) -> ControlFlow<Halt> {
        match action {
            Some(Action::Start) => {
                let purpose = self.practice.clone().map_or(
                    Purpose::Dictation(Handoff::Insert(Enter::Withheld)),
                    Purpose::Practice,
                );
                return self.start_session(now, purpose, env);
            },
            Some(Action::Finish) => {
                self.finish(Recorder::Microphone, env);
                self.finish(Recorder::Screen, env);
                if let Some(session) = &mut self.session {
                    session.finish();
                }
                self.cue(Cue::Stop, env);
            },
            Some(Action::Cancel) => {
                self.discard(env);
                self.cue(Cue::Cancel, env);
            },
            None => {},
        }
        ControlFlow::Continue(())
    }

    fn start_session(
        &mut self,
        now: Now<P>,
        purpose: Purpose<P>,
        env: &mut impl Env<P>,
    ) -> ControlFlow<Halt> {
        let Some((id, permit)) = self.admit(env)? else {
            return ControlFlow::Continue(());
        };
        self.begin(Session::new(id, permit, now.instant, purpose, env.style()));
        ControlFlow::Continue(())
    }

    fn admit(
        &mut self,
        env: &mut impl Env<P>,
    ) -> ControlFlow<Halt, Option<(SessionId, P::Permit)>> {
        let Some(permit) = env.admit() else {
            self.gesture.abandon();
            return ControlFlow::Continue(None);
        };
        ControlFlow::Continue(Some((self.latest.id.next(), permit)))
    }

    fn begin(&mut self, session: Session<P>) {
        self.latest = Latest {
            id: session.id,
            started: session.started,
            show: session.is_show(),
            style: session.style,
        };
        self.outcome = Outcome::Idle;
        self.notice = None;
        self.session = Some(session);
    }

    fn paste_last(&mut self, now: Now<P>, env: &mut impl Env<P>) -> ControlFlow<Halt> {
        if self.practice.is_some() {
            return ControlFlow::Continue(());
        }
        if self.last_dictation.is_none() {
            if self.session.is_none() {
                self.outcome = Outcome::NothingToPaste;
            }
            return ControlFlow::Continue(());
        }
        if matches!(self.gesture.state(), State::Held | State::HandsFree)
            && self.session.as_ref().is_some_and(Session::is_dictation)
        {
            self.abandon(env);
            self.worker.revive(&self.config, env);
        }
        if self.session.is_some() || !self.gesture.deliver() {
            return ControlFlow::Continue(());
        }
        self.start_session(now, Purpose::Replay, env)?;
        if let (Some(session), Some(text)) = (&mut self.session, &self.last_dictation) {
            session.insert(text.clone(), &self.config, env);
        }
        ControlFlow::Continue(())
    }

    fn arm_practice(&mut self, practice: P::Practice, env: &mut impl Env<P>) {
        if self.session.is_some() {
            env.refuse_practice(practice);
        } else if env.ready(&practice) {
            self.practice = Some(practice);
        }
    }

    fn end_practice(&mut self, env: &mut impl Env<P>) {
        self.practice = None;
        if self.session.as_ref().is_some_and(Session::is_practice) {
            self.discard(env);
            self.cue(Cue::Cancel, env);
        }
    }

    fn start_recovery(
        &mut self,
        now: Now<P>,
        interrupted: P::Interrupted,
        receipt: P::Receipt,
        env: &mut impl Env<P>,
    ) -> ControlFlow<Halt> {
        if !self.gesture.save() {
            return ControlFlow::Continue(());
        }
        let Some((id, permit)) = self.admit(env)? else {
            return ControlFlow::Continue(());
        };
        if !env.shield(&permit) {
            self.gesture.abandon();
            return ControlFlow::Continue(());
        }
        env.reopen(interrupted);
        self.begin(Session::recovering(id, permit, now.instant, receipt));
        ControlFlow::Continue(())
    }

    fn settings(&self) -> Settings {
        P::settings(&self.config)
    }

    fn hold_cues(&mut self, env: &mut impl Env<P>) {
        let held = self.settings().play_sounds && self.session.is_some();
        if held != self.cues_held {
            self.cues_held = held;
            env.hold_cues(held);
        }
    }

    fn cue(&self, cue: Cue, env: &mut impl Env<P>) {
        if self.settings().play_sounds {
            env.cue(cue);
        }
    }

    fn cue_heard(&self, cue: Cue, env: &mut impl Env<P>) {
        if self.settings().play_sounds {
            if self.microphone == Device::Open {
                env.expect_cue();
            }
            env.cue(cue);
        }
    }

    fn shape_started(&mut self, style: Option<P::Style>, captured: bool) {
        let Some(session) = &mut self.session else {
            return;
        };
        if let Some(style) = style {
            session.style = style;
            self.latest.style = style;
        }
        if captured {
            session.capture();
        }
    }

    fn print(
        &mut self,
        now: Now<P>,
        call: P::Call,
        token: Option<SessionId>,
        env: &mut impl Env<P>,
    ) -> ControlFlow<Halt> {
        if !self.controls(token) {
            env.respond(call, Err(Refusal::NotActive));
            return ControlFlow::Continue(());
        }
        let call = match &mut self.session {
            Some(session) => match session.print_to(call) {
                Ok(()) => {
                    let action = self.gesture.finish();
                    return self.handle_action(now, action, env);
                },
                Err(call) => call,
            },
            None => call,
        };
        env.respond(call, Err(Refusal::NotCaptured));
        ControlFlow::Continue(())
    }

    fn end_session(&mut self, outcome: Outcome<P>, env: &mut impl Env<P>) {
        if let Some(mut session) = self.session.take() {
            close(&mut session, env);
        }
        self.gesture.complete();
        self.clear_warning();
        self.outcome = outcome;
    }

    fn discard(&mut self, env: &mut impl Env<P>) {
        self.abandon(env);
        self.worker.revive(&self.config, env);
        self.outcome = Outcome::Cancelled;
    }

    fn abandon(&mut self, env: &mut impl Env<P>) {
        self.gesture.abandon();
        self.clear_warning();
        let Some(mut session) = self.session.take() else {
            return;
        };
        close(&mut session, env);
        if session.is_show() {
            self.retire(Recorder::Screen, env);
        }
        if session.is_capturing() {
            self.retire(Recorder::Microphone, env);
        }
        if (session.is_transcribing() && matches!(self.worker, Worker::Transcribing(_)))
            || self.worker.is_streaming(session.id)
        {
            self.worker
                .recover(&self.config, self.settings().probe, env);
        }
        let id = session.id;
        if session.into_cleanup(env) {
            self.retiring.push(id);
        }
    }

    fn fail(&mut self, failure: Failure<P>, env: &mut impl Env<P>) {
        self.abandon(env);
        self.outcome = Outcome::Failed(failure);
    }

    fn advance(&mut self, env: &mut impl Env<P>) {
        self.open_queued_capture(env);
        self.attach_audio(env);
        self.open_queued_screen(env);
        self.start_transcription(env);
        self.start_publishing(env);
    }

    fn warn(&mut self, warning: Notice<P>) {
        if self
            .notice
            .as_ref()
            .is_none_or(|notice| notice.failure().is_none())
        {
            self.notice = Some(warning);
        }
    }

    fn clear_warning(&mut self) {
        if self.notice.as_ref().is_some_and(Notice::is_warning) {
            self.notice = None;
        }
    }

    const fn device(&mut self, recorder: Recorder) -> &mut Device {
        match recorder {
            Recorder::Microphone => &mut self.microphone,
            Recorder::Screen => &mut self.screen,
        }
    }

    fn finish(&mut self, recorder: Recorder, env: &mut impl Env<P>) {
        if *self.device(recorder) == Device::Open {
            env.finish(recorder);
        }
    }

    fn retire(&mut self, recorder: Recorder, env: &mut impl Env<P>) {
        let device = self.device(recorder);
        if *device == Device::Open {
            *device = Device::Retiring;
            env.retire(recorder);
        }
    }

    fn open(&mut self, recorder: Recorder, env: &mut impl Env<P>) {
        *self.device(recorder) = Device::Open;
        if self.gesture.state() == State::Processing {
            env.finish(recorder);
        }
    }

    fn attach_audio(&mut self, env: &mut impl Env<P>) {
        let Some((log, start)) = self.session.as_mut().and_then(Session::take_audio_log) else {
            return;
        };
        let kept = self.microphone == Device::Open && env.keep_audio(log, start, &self.config);
        if !kept {
            self.warn(Notice::CheckpointLost);
        }
    }

    fn open_queued_capture(&mut self, env: &mut impl Env<P>) {
        if self.microphone != Device::Free {
            return;
        }
        let Some(session) = self.session.as_mut().filter(|session| session.is_queued()) else {
            return;
        };
        let mode = if session.is_show() {
            RecognitionMode::Whole
        } else {
            P::settings(&self.config).recognition
        };
        match env.record_microphone(session.id, &self.config, mode) {
            Ok(()) => {
                session.opened();
                self.open(Recorder::Microphone, env);
            },
            Err(text) => self.fail(Failure::Said(text), env),
        }
    }

    fn open_queued_screen(&mut self, env: &mut impl Env<P>) {
        if self.screen != Device::Free {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(scope) = session.queued_screen() else {
            return;
        };
        let frames = session.take_frame_log();
        match env.record_screen(session.id, session.started, scope, frames) {
            Ok(()) => {
                session.screen_opened();
                self.open(Recorder::Screen, env);
            },
            Err(text) => {
                session.forfeit();
                self.fail(Failure::Said(text), env);
            },
        }
    }

    fn start_transcription(&mut self, env: &mut impl Env<P>) {
        if let Some(text) = self.session.as_mut().and_then(Session::complete) {
            self.insert_transcript(text, env);
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.awaits_worker()
            && self.worker.revive(&self.config, env)
            && !matches!(self.notice, Some(Notice::NarrationLost(_)))
        {
            self.notice = Some(Notice::LoadingModel);
        }
        self.worker.transcribe(session, &self.config, env);
    }

    fn start_publishing(&mut self, env: &mut impl Env<P>) {
        if let Some(session) = &mut self.session {
            session.publish(env);
        }
    }

    fn handle_abandoned(&mut self, id: SessionId, delivered: Delivered<P>, env: &mut impl Env<P>) {
        let Some(index) = self.retiring.iter().position(|retiring| *retiring == id) else {
            return;
        };
        self.retiring.remove(index);
        match delivered {
            Delivered::Abandoned(Err(text)) => {
                if self.session.is_none() && self.latest.id == id {
                    self.outcome = Outcome::Failed(Failure::RecoveryStorage(text));
                }
            },
            delivered => {
                if let Some(shown) = saved_anyway(self.remembered, id, delivered) {
                    self.remember(shown, id, env);
                }
            },
        }
    }

    fn control(
        &mut self,
        now: Now<P>,
        call: P::Call,
        command: Command<P>,
        env: &mut impl Env<P>,
    ) -> ControlFlow<Halt> {
        let reply = match command {
            Command::Status => Ok(()),
            Command::Print(token) => return self.print(now, call, token, env),
            Command::Start(style) | Command::Capture(style) => {
                if self.is_busy() {
                    Err(Refusal::Busy)
                } else if !self.desktop_ready || self.worker.model() != ModelState::Ready {
                    Err(Refusal::NotReady)
                } else {
                    let action = self.gesture.toggle(now.moment);
                    self.handle_action(now, action, env)?;
                    self.shape_started(style, matches!(command, Command::Capture(_)));
                    if self.control_session().is_some() {
                        Ok(())
                    } else {
                        Err(Refusal::StartStopped)
                    }
                }
            },
            Command::Stop(token) | Command::Cancel(token) if !self.controls(token) => {
                Err(Refusal::NotActive)
            },
            Command::Stop(_) => {
                let action = self.gesture.finish();
                self.handle_action(now, action, env)?;
                Ok(())
            },
            Command::Cancel(_) => {
                let action = self.gesture.cancel();
                self.handle_action(now, action, env)?;
                Ok(())
            },
            Command::PasteLast => {
                if self.is_busy() {
                    Err(Refusal::PasteBusy)
                } else if !self.desktop_ready {
                    Err(Refusal::PasteNotReady)
                } else if self.last_dictation.is_none() {
                    Err(Refusal::NothingToPaste)
                } else {
                    self.paste_last(now, env)?;
                    if self.session.as_ref().is_some_and(Session::is_replay) {
                        Ok(())
                    } else {
                        Err(Refusal::PasteStopped)
                    }
                }
            },
            Command::Unsupported => Err(Refusal::Unsupported),
        };
        env.respond(call, reply.map(|()| self.status()));
        ControlFlow::Continue(())
    }

    fn adopt_preset(
        &mut self,
        name: P::Text,
        config: P::Config,
        env: &mut impl Env<P>,
    ) -> Result<(), Refusal> {
        if self.is_busy() {
            return Err(Refusal::PresetBusy);
        }
        self.apply_settings(config, env);
        self.outcome = Outcome::Idle;
        self.preset = Some(name);
        Ok(())
    }

    const fn is_busy(&self) -> bool {
        self.session.is_some() || self.practice.is_some()
    }

    fn control_session(&self) -> Option<SessionId> {
        self.session
            .as_ref()
            .filter(|session| session.is_dictation())
            .map(|session| session.id)
    }

    fn controls(&self, token: Option<SessionId>) -> bool {
        token.is_some() && self.control_session() == token
    }
}

fn close<P: Payload>(session: &mut Session<P>, env: &mut impl Env<P>) {
    env.revoke(&session.permit);
    if let Some(call) = session.take_caller() {
        env.respond(call, Err(Refusal::Unheard));
    }
}

fn saved_anyway<P: Payload>(
    remembered: Option<SessionId>,
    session: SessionId,
    delivered: Delivered<P>,
) -> Option<P::Shown> {
    let Delivered::Published(Published::Saved { shown, .. } | Published::Recovered { shown, .. }) =
        delivered
    else {
        return None;
    };
    remembered
        .is_none_or(|remembered| remembered < session)
        .then_some(shown)
}

#[cfg(not(test))]
redacted!(
    Now, Status, Keeping, Event, Input, Report, Command, Capture, Unheard, Lost, Screening,
    Stopped, Completion, Delivered, Inserted, Published, Machine,
);

#[cfg(test)]
mod tests;

#[cfg(test)]
mod explore;
