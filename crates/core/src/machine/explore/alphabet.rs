use super::{
    super::{
        Capture, Command, Completion, Delivered, Device, Event, Input, Inserted, Machine,
        NarrationLoss, Part, Partition, Published, RecognitionMode, Recognized, Recorder, Reopened,
        Report, Screening, Sealed, SessionId, Settings, Stopped, Transcript, Unheard, Worker,
        session::Session,
    },
    Abstract,
    canonical::STALE,
};
use crate::gesture::Moment;

pub(super) enum Step {
    Elapse,
    Event(Event<Abstract>),
}

pub(super) const CONFIGS: [Settings; 3] = [
    config(true, RecognitionMode::Segmented, false),
    config(false, RecognitionMode::Streaming, false),
    config(false, RecognitionMode::Whole, true),
];

const fn config(play_sounds: bool, recognition: RecognitionMode, probe: bool) -> Settings {
    Settings {
        play_sounds,
        recognition,
        probe,
    }
}

struct Steps(Vec<(String, Step)>);

impl Steps {
    fn event(&mut self, label: impl Into<String>, event: Event<Abstract>) {
        self.0.push((label.into(), Step::Event(event)));
    }
}

pub(super) fn steps(
    machine: &Machine<Abstract>,
    clock: Moment,
    mode: Option<RecognitionMode>,
) -> Vec<(String, Step)> {
    let mut steps = Steps(Vec::new());
    if machine.deadline().is_some_and(|deadline| deadline > clock) {
        steps.0.push(("elapse".into(), Step::Elapse));
    }
    steps.event("stop", Event::Stop);
    steps.event("input_lost", Event::InputLost);
    inputs(&mut steps, machine);
    reports(&mut steps);
    for (index, config) in CONFIGS.into_iter().enumerate() {
        steps.event(format!("configure({index})"), Event::Configure(config));
    }
    controls(&mut steps, machine);
    if let Some(session) = &machine.session {
        captures(&mut steps, session, mode);
        screenings(&mut steps, session);
        deliveries(&mut steps, session);
    }
    completions(&mut steps, machine);
    for (index, &id) in machine.retiring.iter().enumerate() {
        abandoned(&mut steps, index, id);
    }
    for (recorder, device) in [
        (Recorder::Microphone, machine.microphone),
        (Recorder::Screen, machine.screen),
    ] {
        if device == Device::Retiring {
            steps.event(format!("retired({recorder:?})"), Event::Retired(recorder));
        }
    }
    if machine.session.is_none() {
        steps.event(
            "recover",
            Event::Recover {
                interrupted: (),
                receipt: (),
            },
        );
    }
    steps.event("practice", Event::Practice(()));
    if machine.practice.is_some() {
        steps.event("practice_closed", Event::PracticeClosed);
    }
    if machine.deadline().is_some_and(|deadline| deadline <= clock) {
        steps.event("timer", Event::Timer);
    }
    steps.0
}

fn inputs(steps: &mut Steps, machine: &Machine<Abstract>) {
    for (label, input) in [
        ("press", Input::Press),
        ("release", Input::Release),
        ("lock", Input::Lock),
        ("show", Input::Show(())),
        ("pin", Input::Pin(())),
        ("escape", Input::Escape),
        ("cancel", Input::Cancel),
        ("interrupt", Input::Interrupt),
        ("toggle", Input::Toggle),
        ("paste_last", Input::PasteLast),
        ("start_show", Input::StartShow(())),
        ("unavailable", Input::Unavailable(())),
    ] {
        steps.event(label, Event::Input(input));
    }
    if !machine.desktop_ready {
        steps.event("desktop_ready", Event::Input(Input::DesktopReady));
    }
}

fn reports(steps: &mut Steps) {
    for (label, report) in [
        ("report(failed)", Report::Failed(())),
        ("report(recurring)", Report::Recurring(())),
        ("report(show_copied)", Report::ShowCopied),
        ("report(show_gone)", Report::ShowGone),
    ] {
        steps.event(label, Event::Report(report));
    }
}

fn controls(steps: &mut Steps, machine: &Machine<Abstract>) {
    let current = machine.latest.id;
    let other = current.next().unwrap_or(STALE);
    for (label, command) in [
        ("control(status)", Command::Status),
        ("control(start)", Command::Start(None)),
        ("control(start_styled)", Command::Start(Some(()))),
        ("control(stop)", Command::Stop(Some(current))),
        ("control(stop_other)", Command::Stop(Some(other))),
        ("control(stop_unknown)", Command::Stop(None)),
        ("control(cancel)", Command::Cancel(Some(current))),
        ("control(cancel_other)", Command::Cancel(Some(other))),
        ("control(cancel_unknown)", Command::Cancel(None)),
        ("control(paste_last)", Command::PasteLast),
        ("control(unsupported)", Command::Unsupported),
    ] {
        steps.event(label, Event::Control { call: (), command });
    }
    for (index, config) in CONFIGS.into_iter().enumerate() {
        steps.event(
            format!("preset({index})"),
            Event::Preset {
                call: (),
                name: (),
                config,
                session: current,
                changed: false,
            },
        );
    }
    for (label, session, changed) in [
        ("preset(other)", other, false),
        ("preset(changed)", current, true),
    ] {
        steps.event(
            label,
            Event::Preset {
                call: (),
                name: (),
                config: CONFIGS[0],
                session,
                changed,
            },
        );
    }
}

fn captures(steps: &mut Steps, session: &Session<Abstract>, mode: Option<RecognitionMode>) {
    let current = session.id;
    steps.event(
        "capture(stale,ready)",
        Event::Capture(STALE, Capture::Ready),
    );
    steps.event(
        "capture(stale,finished)",
        Event::Capture(STALE, Capture::Finished(Ok(None))),
    );
    let Some(mode) = mode else {
        return;
    };
    let sealed = |partition| {
        Capture::Finished(Ok(Some(Sealed {
            audio: (),
            trimmed: (),
            partition,
        })))
    };
    let mut captures = vec![
        ("ready".to_owned(), Capture::Ready),
        ("level".to_owned(), Capture::Level(())),
        ("finished(nothing)".to_owned(), Capture::Finished(Ok(None))),
        ("finished(whole)".to_owned(), sealed(None)),
    ];
    if session.is_show() {
        captures.push(("checkpoint_lost".to_owned(), Capture::CheckpointLost));
    }
    match mode {
        RecognitionMode::Streaming => {
            captures.push(("streaming".to_owned(), Capture::Streaming(())));
        },
        RecognitionMode::Segmented => {
            captures.push(("piece(0)".to_owned(), Capture::Piece(0, ())));
            captures.push(("piece(1)".to_owned(), Capture::Piece(1, ())));
            for pieces in 0..=2 {
                for empty in [false, true] {
                    captures.push((
                        format!("finished({pieces},{empty})"),
                        sealed(Some(Partition {
                            pieces,
                            tail: (),
                            empty,
                        })),
                    ));
                }
            }
        },
        RecognitionMode::Whole => {},
    }
    for silent in [false, true] {
        captures.push((
            format!("failed({silent})"),
            Capture::Finished(Err(Unheard { silent, text: () })),
        ));
    }
    for (label, capture) in captures {
        steps.event(
            format!("capture({label})"),
            Event::Capture(current, capture),
        );
    }
}

fn screenings(steps: &mut Steps, session: &Session<Abstract>) {
    steps.event(
        "screen(stale,finished)",
        Event::Show(STALE, Screening::Finished(Ok(()))),
    );
    if !session.records_screen() {
        return;
    }
    for (label, screening) in [
        ("checkpoint_lost", Screening::CheckpointLost),
        ("movie_stopped", Screening::MovieStopped),
        ("finished", Screening::Finished(Ok(()))),
        (
            "stopped",
            Screening::Finished(Err(Stopped {
                unwound: false,
                text: (),
            })),
        ),
        (
            "unwound",
            Screening::Finished(Err(Stopped {
                unwound: true,
                text: (),
            })),
        ),
        ("unchosen", Screening::Unchosen(())),
    ] {
        steps.event(
            format!("screen({label})"),
            Event::Show(session.id, screening),
        );
    }
}

fn deliveries(steps: &mut Steps, session: &Session<Abstract>) {
    let mut delivered = Vec::new();
    if session.is_reopening() {
        for incomplete in [false, true] {
            delivered.push((
                format!("reopened(speech,{incomplete})"),
                Delivered::Reopened(Ok(reopened(Recognized::Speech {
                    audio: (),
                    start: (),
                    incomplete,
                }))),
            ));
        }
        for loss in [
            None,
            Some(NarrationLoss::Incomplete),
            Some(NarrationLoss::AudioStopped),
        ] {
            delivered.push((
                format!("reopened(narration,{loss:?})"),
                Delivered::Reopened(Ok(reopened(Recognized::Narration {
                    narration: (),
                    loss,
                }))),
            ));
        }
        delivered.push(("reopened(failed)".into(), Delivered::Reopened(Err(()))));
    } else if session.is_delivering() && session.is_show() {
        delivered.extend(published());
    } else if session.is_delivering() {
        for (label, inserted) in [
            ("sent", Inserted::Sent),
            ("copied", Inserted::Copied(())),
            ("unavailable", Inserted::Unavailable(())),
            ("cancelled", Inserted::Cancelled),
            ("failed", Inserted::Failed),
        ] {
            delivered.push((format!("inserted({label})"), Delivered::Inserted(inserted)));
        }
    } else {
        return;
    }
    delivered.push(("panicked".into(), Delivered::Panicked));
    for (label, delivered) in delivered {
        steps.event(format!("delivered({label})"), Event::Delivered(delivered));
    }
}

const fn reopened(recognized: Recognized<Abstract>) -> Reopened<Abstract> {
    Reopened {
        footage: (),
        checkpoint: (),
        pending: (),
        recognized,
    }
}

fn published() -> Vec<(String, Delivered<Abstract>)> {
    vec![
        (
            "saved".into(),
            Delivered::Published(Published::Saved {
                shown: (),
                saved: (),
                copied: Ok(()),
            }),
        ),
        (
            "saved_uncopied".into(),
            Delivered::Published(Published::Saved {
                shown: (),
                saved: (),
                copied: Err(()),
            }),
        ),
        (
            "recovered".into(),
            Delivered::Published(Published::Recovered {
                shown: (),
                recovered: (),
            }),
        ),
        (
            "publish_cancelled".into(),
            Delivered::Published(Published::Cancelled),
        ),
        (
            "publish_failed".into(),
            Delivered::Published(Published::Failed(())),
        ),
    ]
}

fn completions(steps: &mut Steps, machine: &Machine<Abstract>) {
    let mut completions = Vec::new();
    match machine.worker {
        Worker::Loading => {
            completions.push(("loaded".to_owned(), Completion::Loaded(Ok(()))));
            completions.push(("load_failed".to_owned(), Completion::Loaded(Err(()))));
        },
        Worker::Transcribing(ticket) => {
            let words = ticket.part == Part::Final
                && machine
                    .session
                    .as_ref()
                    .is_some_and(|session| session.id == ticket.session && session.is_show());
            let transcript = if words {
                ("words", Transcript::Words(()))
            } else {
                ("text", Transcript::Text(()))
            };
            completions.push((
                transcript.0.to_owned(),
                Completion::Transcribed(ticket, Ok(transcript.1)),
            ));
            completions.push((
                "failed".to_owned(),
                Completion::Transcribed(ticket, Err(())),
            ));
            if ticket.part == Part::Stream {
                completions.push(("abandoned".to_owned(), Completion::Abandoned(ticket)));
            }
        },
        Worker::Unavailable | Worker::Ready => {},
    }
    for (label, completion) in completions {
        steps.event(format!("worker({label})"), Event::Worker(completion));
    }
}

fn abandoned(steps: &mut Steps, index: usize, id: SessionId) {
    let mut abandoned = vec![
        ("released".to_owned(), Delivered::Abandoned(Ok(()))),
        ("release_failed".to_owned(), Delivered::Abandoned(Err(()))),
        ("inserted".to_owned(), Delivered::Inserted(Inserted::Sent)),
        ("reopened(failed)".to_owned(), Delivered::Reopened(Err(()))),
        ("panicked".to_owned(), Delivered::Panicked),
    ];
    abandoned.extend(published());
    for (label, delivered) in abandoned {
        steps.event(
            format!("abandoned({index},{label})"),
            Event::Abandoned(id, delivered),
        );
    }
}
