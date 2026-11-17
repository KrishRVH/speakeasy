mod commands;
mod outcome;

use std::{
    convert::Infallible,
    future::{pending, poll_fn},
    mem,
    pin::Pin,
    sync::Arc,
    task::Poll,
    time::{Duration, Instant, SystemTime},
};

use async_channel::{Receiver, RecvError, Sender};
use speakeasy_core::{
    gesture::{Moment, Timing},
    machine::{
        self, Capture, Env, Event, Failure, Flow, Keeping, Kept, Machine, Now, Outcome,
        Publication, RecognitionMode, Recorder, Refusal, Release, Request, Screening, SessionId,
        Status, Stopped, Unheard,
    },
};
use speakeasy_platform::{Input, InputSender, InsertPermit, Scope, Taps};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinError, JoinHandle},
    time::sleep_until,
};
use zeroize::Zeroizing;

use super::{
    Directive, Meter, PracticeEvent, Recovery, Report, Snapshot, SnapshotEpoch, Wiring,
    device::Device,
    native::{self, Clip, Delivery, Native},
    now,
    ports::{CaptureEvent, Ports, Published, ShowEvent},
    worker::{Completion, Worker},
};
use crate::{
    audio::SilentMicrophone,
    config::Config,
    show::{self, Checkpointing, SavedShow, Unwound},
    transcript::{self, style::Style},
};

enum Wake<W> {
    Stop,
    Input(Result<Input, RecvError>),
    Report(Report),
    Configuration,
    Command(Directive),
    Capture(CaptureEvent),
    Show(ShowEvent),
    Worker(Completion<W>),
    Delivered(Result<Delivery, JoinError>),
    Abandoned(SessionId, Result<Delivery, JoinError>),
    Retired(Recorder),
    Recover(Recovery),
    Practice(mpsc::Sender<PracticeEvent>),
    EndPractice,
    Timer,
}

struct Owner<P: Ports> {
    machine: Machine<Native>,
    world: World<P>,
}

struct World<P: Ports> {
    ports: P,
    origin: Instant,
    changes: watch::Receiver<Config>,
    styles: watch::Receiver<Style>,
    commands: Receiver<Directive>,
    incarnation: u64,
    inputs: Receiver<Input>,
    reports: Receiver<Report>,
    recoveries: Receiver<Recovery>,
    practices: Receiver<mpsc::Sender<PracticeEvent>>,
    stopping: Receiver<Infallible>,
    capture_events: Sender<CaptureEvent>,
    captures: Receiver<CaptureEvent>,
    show_events: Sender<ShowEvent>,
    shows: Receiver<ShowEvent>,
    permits: InputSender,
    last_show: Option<SavedShow>,
    reported: u64,
    worker: Worker<P::Speech>,
    microphone: Device<P::Microphone>,
    screen: Device<P::Screen>,
    delivery: Option<JoinHandle<Delivery>>,
    retiring: Vec<(SessionId, JoinHandle<Delivery>)>,
    snapshots: watch::Sender<Snapshot>,
    meter: watch::Sender<Meter>,
    epoch: SnapshotEpoch,
    shortcut: Arc<str>,
    cancel_shortcut: Arc<str>,
}

impl<P: Ports> Owner<P> {
    fn new(wiring: Wiring<P>) -> Self {
        let Wiring {
            config,
            changes,
            styles,
            commands,
            incarnation,
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
        let (capture_events, captures) = async_channel::bounded(64);
        let (show_events, shows) = async_channel::bounded(4);
        let desktop_ready = !ports.prepares_desktop();
        let origin = now();
        let preset = snapshots.borrow().preset.clone();
        let mut world = World {
            ports,
            origin,
            changes,
            styles,
            commands,
            incarnation,
            inputs,
            reports,
            recoveries,
            practices,
            stopping,
            capture_events,
            captures,
            show_events,
            shows,
            permits,
            last_show,
            reported: 0,
            worker: Worker::default(),
            microphone: Device::Free,
            screen: Device::Free,
            delivery: None,
            retiring: Vec::new(),
            snapshots,
            meter,
            epoch,
            shortcut: speakeasy_platform::SHORTCUT.into(),
            cancel_shortcut: speakeasy_platform::CANCEL_SHORTCUT.into(),
        };
        let machine = Machine::new(
            Timing::DICTATION,
            config,
            desktop_ready,
            origin,
            preset,
            &mut world,
        );
        Self { machine, world }
    }

    async fn wait(&mut self) -> Wake<P::Speech> {
        let world = &mut self.world;
        let origin = world.origin;
        let timer = or_pending(self.machine.deadline().and_then(|deadline| {
            let at = origin.checked_add(Duration::from_nanos(deadline.nanos()))?;
            Some(sleep_until(at.into()))
        }));
        let idle = self.machine.is_idle();
        let practice_closed = or_pending(self.machine.practice().map(mpsc::Sender::closed));
        let delivered = or_pending(world.delivery.as_mut());

        tokio::select! {
            biased;
            _ = world.stopping.recv() => Wake::Stop,
            () = practice_closed => Wake::EndPractice,
            Ok(practice) = world.practices.recv() => Wake::Practice(practice),
            input = world.inputs.recv() => Wake::Input(input),
            report = world.reports.recv() => report.map_or(Wake::Stop, Wake::Report),
            change = world.changes.changed() => change.map_or(Wake::Stop, |()| Wake::Configuration),
            Ok(command) = world.commands.recv() => Wake::Command(command),
            Ok(event) = world.captures.recv() => Wake::Capture(event),
            Ok(event) = world.shows.recv() => Wake::Show(event),
            Ok(recovery) = world.recoveries.recv(), if idle => Wake::Recover(recovery),
            () = world.microphone.retired() => Wake::Retired(Recorder::Microphone),
            () = world.screen.retired() => Wake::Retired(Recorder::Screen),
            completion = world.worker.completed() => Wake::Worker(completion),
            delivered = delivered => Wake::Delivered(delivered),
            (session, finished) = next_finished(&mut world.retiring) => {
                Wake::Abandoned(session, finished)
            },
            () = timer => Wake::Timer,
        }
    }

    fn event(&mut self, wake: Wake<P::Speech>) -> Option<Event<Native>> {
        let world = &mut self.world;
        Some(match wake {
            Wake::Stop => Event::Stop,
            Wake::Input(Err(_)) => Event::InputLost,
            Wake::Input(Ok(input)) => Event::Input(world.input(input)),
            Wake::Report(report) => {
                report.forget_gone(&mut world.last_show);
                Event::Report(match report {
                    Report::Failed(message) => machine::Report::Failed(message),
                    Report::Recurring(message) => machine::Report::Recurring(message),
                    Report::ShowCopied => machine::Report::ShowCopied,
                    Report::ShowGone(_) => machine::Report::ShowGone,
                })
            },
            Wake::Configuration => Event::Configure(world.changes.borrow_and_update().clone()),
            Wake::Command(directive) => return world.direct(directive),
            Wake::Capture(event) => capture(event),
            Wake::Show(event) => screening(event),
            Wake::Worker(completion) => Event::Worker(world.completed(completion)),
            Wake::Delivered(result) => {
                world.delivery = None;
                Event::Delivered(Delivery::settled(result))
            },
            Wake::Abandoned(session, result) => {
                Event::Abandoned(session, Delivery::settled(result))
            },
            Wake::Retired(recorder) => Event::Retired(recorder),
            Wake::Recover(Recovery {
                interrupted,
                completed,
            }) => Event::Recover {
                interrupted,
                receipt: completed,
            },
            Wake::Practice(practice) => Event::Practice(practice),
            Wake::EndPractice => Event::PracticeClosed,
            Wake::Timer => Event::Timer,
        })
    }

    fn now(&self) -> Now<Native> {
        let instant = now();
        Now {
            moment: Moment::since(self.world.origin, instant),
            instant,
        }
    }

    fn present(&self) {
        let machine = &self.machine;
        let world = &self.world;
        let (ending, narration_lost) = match machine.outcome() {
            Outcome::ShowCopied(copied) => (copied.ending, copied.preservation.narration_lost),
            _ => (speakeasy_show::Ending::Finished, None),
        };
        let snapshot = Snapshot {
            epoch: world.epoch,
            session: machine.session().into(),
            reported: world.reported,
            phase: machine.phase(),
            hands_free: machine.hands_free(),
            show: machine.shows(),
            ending,
            narration_lost,
            started: machine.started(),
            last_show: world.last_show.clone(),
            last_dictation: machine.last_dictation().is_some(),
            style: machine.style(),
            preset: machine.preset().cloned(),
            message: outcome::message(machine.outcome(), machine.notice()),
            model: machine.model(),
            desktop_ready: machine.desktop_ready(),
            shortcut: Arc::clone(&world.shortcut),
            cancel_shortcut: Arc::clone(&world.cancel_shortcut),
        };
        world.snapshots.send_if_modified(|presented| {
            if presented.epoch != world.epoch || *presented == snapshot {
                return false;
            }
            *presented = snapshot;
            true
        });
    }

    async fn shutdown(mut self) {
        self.machine.shut_down(&mut self.world);
        let World {
            worker,
            microphone,
            screen,
            retiring,
            snapshots,
            ..
        } = self.world;
        let retired = async {
            let mut finished = Vec::with_capacity(retiring.len());
            for (session, task) in retiring {
                finished.push((session, task.await));
            }
            finished
        };
        let ((), (), (), finished) =
            tokio::join!(worker.stop(), microphone.stop(), screen.stop(), retired);
        for (session, result) in finished {
            if let Some(show) = self.machine.drained(session, Delivery::settled(result)) {
                keep_presented(&snapshots, &show);
            }
        }
    }
}

impl<P: Ports> World<P> {
    fn input(&mut self, input: Input) -> machine::Input<Native> {
        match input {
            Input::Press => machine::Input::Press,
            Input::Release => machine::Input::Release,
            Input::Lock => machine::Input::Lock,
            Input::Show(scope) => machine::Input::Show(scope),
            Input::Pin(taps) => machine::Input::Pin(taps),
            Input::Escape => machine::Input::Escape,
            Input::Cancel => machine::Input::Cancel,
            Input::Interrupt => machine::Input::Interrupt,
            Input::Toggle => machine::Input::Toggle,
            Input::PasteLast => machine::Input::PasteLast,
            Input::StartShow(scope) => machine::Input::StartShow(scope),
            Input::Unavailable(message) => machine::Input::Unavailable(message),
            Input::DesktopReady { shortcut, cancel } => {
                self.shortcut = shortcut.into();
                self.cancel_shortcut = cancel.into();
                machine::Input::DesktopReady
            },
        }
    }

    fn completed(&mut self, completion: Completion<P::Speech>) -> machine::Completion<Native> {
        match completion {
            Completion::Abandoned(ticket, worker) => {
                self.worker = Worker::Ready(worker);
                machine::Completion::Abandoned(ticket)
            },
            Completion::Loaded(Ok(worker)) => {
                self.worker = Worker::Ready(worker);
                machine::Completion::Loaded(Ok(()))
            },
            Completion::Loaded(Err(error)) => machine::Completion::Loaded(Err(error.to_string())),
            Completion::Transcribed(ticket, Ok((worker, transcript))) => {
                self.worker = Worker::Ready(worker);
                machine::Completion::Transcribed(ticket, Ok(transcript))
            },
            Completion::Transcribed(ticket, Err(error)) => {
                machine::Completion::Transcribed(ticket, Err(error.to_string()))
            },
        }
    }
}

impl<P: Ports> Env<Native> for World<P> {
    fn admit(&mut self) -> Option<InsertPermit> {
        self.permits.begin()
    }

    fn style(&mut self) -> Style {
        *self.styles.borrow()
    }

    fn shield(&mut self, permit: &InsertPermit) -> bool {
        permit.shield()
    }

    fn revoke(&mut self, permit: &InsertPermit) {
        permit.revoke();
    }

    fn closed(&mut self) -> bool {
        self.permits.is_closed()
    }

    fn keep(&mut self, started: Instant) -> Keeping<Native> {
        let recorded = SystemTime::now()
            .checked_sub(now().saturating_duration_since(started))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        match self.ports.keep(recorded) {
            Ok(Some(kept)) => Keeping::Kept(Kept {
                checkpoint: Checkpointing::Writing(kept.keeper),
                audio: kept.audio,
                frames: kept.frames,
            }),
            Ok(None) => Keeping::Unneeded,
            Err(_) => Keeping::Lost,
        }
    }

    fn record_microphone(
        &mut self,
        session: SessionId,
        config: &Config,
        mode: RecognitionMode,
    ) -> Result<(), String> {
        let recording = self
            .ports
            .record_microphone(
                session,
                config.microphone.as_deref(),
                mode,
                self.capture_events.clone(),
            )
            .map_err(|error| format!("Cannot open the microphone ({error}). Try again."))?;
        self.microphone.open(recording);
        Ok(())
    }

    fn record_screen(
        &mut self,
        session: SessionId,
        started: Instant,
        scope: Scope,
        frames: Option<show::FrameLog>,
    ) -> Result<(), String> {
        let recorder = self
            .ports
            .record_screen(session, started, scope, frames, self.show_events.clone())
            .map_err(|error| format!("Cannot record the screen: {error}"))?;
        self.screen.open(recorder);
        Ok(())
    }

    fn finish(&mut self, recorder: Recorder) {
        match recorder {
            Recorder::Microphone => self.microphone.finish(),
            Recorder::Screen => self.screen.finish(),
        }
    }

    fn retire(&mut self, recorder: Recorder) {
        match recorder {
            Recorder::Microphone => self.microphone.retire(),
            Recorder::Screen => self.screen.retire(),
        }
    }

    fn listen_whole(&mut self) {
        self.microphone.whole();
    }

    fn expect_cue(&mut self) {
        self.microphone.expect_cue();
    }

    fn keep_audio(&mut self, log: show::AudioLog, start: Duration, config: &Config) -> bool {
        self.microphone.keep(log, start, &config.language)
    }

    fn pin(&mut self, taps: Taps) {
        self.screen.pin(taps);
    }

    fn meter(&mut self, session: SessionId, level: f32) {
        self.meter.send_replace(Meter {
            epoch: self.epoch,
            session: session.into(),
            level,
        });
    }

    fn load(&mut self, config: &Config) {
        self.worker = Worker::load(&self.ports, config);
    }

    fn reload(&mut self, config: &Config) {
        self.worker.reload(&self.ports, config);
    }

    fn recover(&mut self, config: &Config, probe: bool) {
        self.worker.recover(&self.ports, config, probe);
    }

    fn cut(&mut self, audio: &Clip, range: &std::ops::Range<usize>) -> Option<Clip> {
        let wav = crate::audio::wav_tail(&audio.wav, range.clone()).ok()?;
        Some(Clip {
            wav: Zeroizing::new(wav),
            language: audio.language.clone(),
            audible: audio.audible,
        })
    }

    fn transcribe(&mut self, request: Request<Native>, config: &Config) {
        self.worker.transcribe(config, request);
    }

    fn compose(
        &mut self,
        heard: Zeroizing<String>,
        style: Style,
        config: &Config,
    ) -> Option<Zeroizing<String>> {
        let text = transcript::for_dictation(
            &heard,
            style,
            config.fillers(&config.language),
            &config.vocabulary,
        );
        (!text.is_empty()).then(|| Zeroizing::new(text))
    }

    fn insert(&mut self, mut text: Zeroizing<String>, permit: &InsertPermit, config: &Config) {
        let insertion = self
            .ports
            .insert(mem::take(&mut *text), permit.clone(), config.delivery());
        self.delivery = Some(tokio::spawn(
            async move { Delivery::Inserted(insertion.await) },
        ));
    }

    fn ready(&mut self, practice: &mpsc::Sender<PracticeEvent>) -> bool {
        practice.try_send(PracticeEvent::Ready).is_ok()
    }

    fn refuse_practice(&mut self, practice: mpsc::Sender<PracticeEvent>) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A closed practice view no longer wants its rejection"
        )]
        let _ = practice.try_send(PracticeEvent::Unavailable(
            "Finish the current recording before trying dictation here.",
        ));
    }

    fn practise(
        &mut self,
        practice: &mpsc::Sender<PracticeEvent>,
        permit: &InsertPermit,
        text: Zeroizing<String>,
    ) -> bool {
        !practice.is_closed()
            && permit.commit()
            && practice.try_send(PracticeEvent::Text(text)).is_ok()
    }

    fn publish(
        &mut self,
        publication: Publication<Native>,
        permit: &InsertPermit,
        receipt: Option<async_channel::Sender<Result<SavedShow, String>>>,
    ) {
        let Publication {
            mut footage,
            narration,
            loss,
            checkpoint,
            origin,
        } = publication;
        footage.recording.narration = narration;
        footage.recording.narration_lost = loss.map(native::show_loss);
        let publication = self.ports.publish(
            show::Publication {
                footage: *footage,
                checkpoint,
                origin: match origin {
                    machine::Origin::Live => show::Origin::Live,
                    machine::Origin::Recovered { pending } => show::Origin::Recovered { pending },
                },
            },
            permit.clone(),
        );
        self.delivery = Some(tokio::spawn(async move {
            let result = publication.await;
            if let Some(receipt) = receipt {
                let completed = match &result {
                    Ok(Published::Recovered { show, .. } | Published::Saved { show, .. }) => {
                        Ok(show.clone())
                    },
                    Ok(Published::Cancelled) => {
                        Err("Recovery cancelled. The interrupted Show is still available.".into())
                    },
                    Err(error) => Err(format!("Cannot recover this Show: {error:#}")),
                };
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Closing the library abandons its receipt, not the owner's recovery"
                )]
                let _ = receipt.try_send(completed);
            }
            Delivery::Published(result)
        }));
    }

    fn reopen(&mut self, interrupted: show::Interrupted) {
        let work = self.ports.reopen(interrupted);
        self.delivery = Some(tokio::spawn(async move {
            Delivery::Reopened(work.await.map(Box::new))
        }));
    }

    fn refuse_recovery(
        &mut self,
        receipt: async_channel::Sender<Result<SavedShow, String>>,
        failure: &Failure<Native>,
    ) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A closed library no longer waits for its failure notice"
        )]
        let _ = receipt.try_send(Err(format!(
            "Cannot recover this Show: {}",
            outcome::failure(failure)
        )));
    }

    fn retire_delivery(&mut self, session: SessionId) {
        if let Some(task) = self.delivery.take() {
            self.retiring.push((session, task));
        }
    }

    fn release(&mut self, session: SessionId, checkpoint: Checkpointing, release: Release<Native>) {
        let release = match release {
            Release::Keep(narration) => show::Release::Keep(narration),
            Release::Discard => show::Release::Discard,
        };
        self.retiring.push((
            session,
            tokio::spawn(async move { Delivery::Abandoned(checkpoint.release(release).await) }),
        ));
    }

    fn remember(&mut self, shown: SavedShow) {
        keep_presented(&self.snapshots, &shown);
        self.last_show = Some(shown);
    }

    fn announce(&mut self) {
        self.reported = self.reported.wrapping_add(1);
    }

    fn cue(&mut self, cue: machine::Cue) {
        self.ports.cue(cue);
    }

    fn hold_cues(&mut self, held: bool) {
        self.ports.hold_cues(held);
    }

    fn respond(&mut self, call: crate::control::Call, reply: Result<Status<Native>, Refusal>) {
        let reply = match reply {
            Ok(status) => crate::control::Reply::ok(self.status(status)),
            Err(refusal) => crate::control::Reply::refused(commands::refusal(refusal)),
        };
        call.respond(reply);
    }

    fn close_input(&mut self) {
        self.permits.close();
        self.captures.close();
        self.shows.close();
    }
}

pub(super) async fn run<P: Ports>(wiring: Wiring<P>) {
    let mut owner = Owner::new(wiring);
    owner.present();
    loop {
        let wake = owner.wait().await;
        let Some(event) = owner.event(wake) else {
            continue;
        };
        let now = owner.now();
        match owner.machine.step(now, event, &mut owner.world) {
            Flow::Running => owner.present(),
            Flow::Failed => {
                owner.present();
                break;
            },
            Flow::Stopped => break,
        }
    }
    owner.shutdown().await;
}

fn capture(event: CaptureEvent) -> Event<Native> {
    let (session, capture) =
        match event {
            CaptureEvent::Streaming(session, audio) => (session, Capture::Streaming(audio)),
            CaptureEvent::Ready(session) => (session, Capture::Ready),
            CaptureEvent::CheckpointLost(session) => (session, Capture::CheckpointLost),
            CaptureEvent::Level(session, level) => (session, Capture::Level(level)),
            CaptureEvent::Piece(session, number, audio) => {
                (session, Capture::Piece(number, Clip::heard(audio, None)))
            },
            CaptureEvent::Finished(session, result) => (
                session,
                Capture::Finished(result.map(|sealed| sealed.map(native::sealed)).map_err(
                    |error| Unheard {
                        silent: error.is::<SilentMicrophone>(),
                        text: error.to_string(),
                    },
                )),
            ),
            CaptureEvent::Lost(session, sealed, lost) => (
                session,
                Capture::Lost(
                    native::sealed(sealed),
                    machine::Lost {
                        notice: lost.notice.into(),
                        failure: lost.failure.to_string(),
                    },
                ),
            ),
        };
    Event::Capture(session, capture)
}

fn screening(event: ShowEvent) -> Event<Native> {
    let (session, screening) = match event {
        ShowEvent::CheckpointLost(session) => (session, Screening::CheckpointLost),
        ShowEvent::MovieStopped(session) => (session, Screening::MovieStopped),
        ShowEvent::Finished(session, result) => (
            session,
            Screening::Finished(result.map_err(|error| Stopped {
                unwound: error.is::<Unwound>(),
                text: format!("{error:#}"),
            })),
        ),
        ShowEvent::Unchosen(session, unchosen) => (session, Screening::Unchosen(unchosen)),
    };
    Event::Show(session, screening)
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
        None => pending().await,
    }
}
