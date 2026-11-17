use super::{
    super::{
        Capture, Command, Completion, Delivered, Device, Event, Input, Inserted, Lost, Machine,
        NarrationLoss, Part, Partition, Published, RecognitionMode, Recognized, Recorder, Reopened,
        Report, Screening, Sealed, SessionId, Settings, Stopped, Transcript, Unheard, Worker,
        session::Session,
    },
    Abstract,
    canonical::STALE,
};
use crate::gesture::Moment;

type Events = Vec<Event<Abstract>>;

#[derive(Clone, Debug)]
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

pub(super) fn steps(
    machine: &Machine<Abstract>,
    clock: Moment,
    mode: Option<RecognitionMode>,
) -> Vec<Step> {
    let mut events = vec![Event::Stop, Event::InputLost];
    inputs(&mut events, machine);
    events.extend(
        [
            Report::Failed(()),
            Report::Recurring(()),
            Report::ShowCopied,
            Report::ShowGone,
        ]
        .map(Event::Report),
    );
    events.extend(CONFIGS.map(Event::Configure));
    controls(&mut events, machine);
    if let Some(session) = &machine.session {
        captures(&mut events, session, mode);
        screenings(&mut events, session);
        deliveries(&mut events, session);
    }
    completions(&mut events, machine);
    for &id in &machine.retiring {
        abandoned(&mut events, id);
    }
    for (recorder, device) in [
        (Recorder::Microphone, machine.microphone),
        (Recorder::Screen, machine.screen),
    ] {
        if device == Device::Retiring {
            events.push(Event::Retired(recorder));
        }
    }
    if machine.session.is_none() {
        events.push(Event::Recover {
            interrupted: (),
            receipt: (),
        });
    }
    events.push(Event::Practice(()));
    if machine.practice.is_some() {
        events.push(Event::PracticeClosed);
    }
    let deadline = machine.deadline();
    if deadline.is_some_and(|deadline| deadline <= clock) {
        events.push(Event::Timer);
    }
    let elapse = deadline.is_some_and(|deadline| deadline > clock);
    elapse
        .then_some(Step::Elapse)
        .into_iter()
        .chain(events.into_iter().map(Step::Event))
        .collect()
}

fn inputs(events: &mut Events, machine: &Machine<Abstract>) {
    events.extend(
        [
            Input::Press,
            Input::Release,
            Input::Lock,
            Input::Show(()),
            Input::Pin(()),
            Input::Escape,
            Input::Cancel,
            Input::Interrupt,
            Input::Toggle,
            Input::PasteLast,
            Input::StartShow(()),
            Input::Unavailable(()),
        ]
        .map(Event::Input),
    );
    if !machine.desktop_ready {
        events.push(Event::Input(Input::DesktopReady));
    }
}

fn controls(events: &mut Events, machine: &Machine<Abstract>) {
    let current = machine.latest.id;
    let other = current.next().unwrap_or(STALE);
    events.extend(
        [
            Command::Status,
            Command::Start(None),
            Command::Start(Some(())),
            Command::Stop(Some(current)),
            Command::Stop(Some(other)),
            Command::Stop(None),
            Command::Cancel(Some(current)),
            Command::Cancel(Some(other)),
            Command::Cancel(None),
            Command::PasteLast,
            Command::Unsupported,
        ]
        .map(|command| Event::Control { call: (), command }),
    );
    let preset = |config, session, changed| Event::Preset {
        call: (),
        name: (),
        config,
        session,
        changed,
    };
    events.extend(CONFIGS.map(|config| preset(config, current, false)));
    events.push(preset(CONFIGS[0], other, false));
    events.push(preset(CONFIGS[0], current, true));
}

const fn sealed(partition: Option<Partition<Abstract>>) -> Sealed<Abstract> {
    Sealed {
        audio: (),
        trimmed: (),
        partition,
    }
}

const fn lost(partition: Option<Partition<Abstract>>) -> Capture<Abstract> {
    Capture::Lost(
        sealed(partition),
        Lost {
            notice: (),
            failure: (),
        },
    )
}

fn captures(events: &mut Events, session: &Session<Abstract>, mode: Option<RecognitionMode>) {
    events.extend(
        [Capture::Ready, Capture::Finished(Ok(None)), lost(None)]
            .map(|capture| Event::Capture(STALE, capture)),
    );
    let Some(mode) = mode else {
        return;
    };
    let finished = |partition| Capture::Finished(Ok(Some(sealed(partition))));
    let mut captures = vec![
        Capture::Ready,
        Capture::Level(()),
        Capture::Finished(Ok(None)),
        finished(None),
        lost(None),
    ];
    if session.is_show() {
        captures.push(Capture::CheckpointLost);
    }
    match mode {
        RecognitionMode::Streaming => captures.push(Capture::Streaming(())),
        RecognitionMode::Segmented => {
            captures.extend([Capture::Piece(0, ()), Capture::Piece(1, ())]);
            for pieces in 0..=2 {
                for empty in [false, true] {
                    let partition = || {
                        Some(Partition {
                            pieces,
                            tail: (),
                            empty,
                        })
                    };
                    captures.extend([finished(partition()), lost(partition())]);
                }
            }
        },
        RecognitionMode::Whole => {},
    }
    captures
        .extend([false, true].map(|silent| Capture::Finished(Err(Unheard { silent, text: () }))));
    events.extend(
        captures
            .into_iter()
            .map(|capture| Event::Capture(session.id, capture)),
    );
}

fn screenings(events: &mut Events, session: &Session<Abstract>) {
    events.push(Event::Show(STALE, Screening::Finished(Ok(()))));
    if !session.records_screen() {
        return;
    }
    let stopped = |unwound| Screening::Finished(Err(Stopped { unwound, text: () }));
    events.extend(
        [
            Screening::CheckpointLost,
            Screening::MovieStopped,
            Screening::Finished(Ok(())),
            stopped(false),
            stopped(true),
            Screening::Unchosen(()),
        ]
        .map(|screening| Event::Show(session.id, screening)),
    );
}

fn deliveries(events: &mut Events, session: &Session<Abstract>) {
    let mut delivered = Vec::new();
    if session.is_reopening() {
        for incomplete in [false, true] {
            delivered.push(reopened(Recognized::Speech {
                audio: (),
                start: (),
                incomplete,
            }));
        }
        for loss in [
            None,
            Some(NarrationLoss::Incomplete),
            Some(NarrationLoss::AudioStopped),
        ] {
            delivered.push(reopened(Recognized::Narration {
                narration: (),
                loss,
            }));
        }
        delivered.push(Delivered::Reopened(Err(())));
    } else if session.is_delivering() && session.is_show() {
        delivered.extend(published());
    } else if session.is_delivering() {
        delivered.extend(
            [
                Inserted::Sent,
                Inserted::Copied(()),
                Inserted::Unavailable(()),
                Inserted::Cancelled,
                Inserted::Failed,
            ]
            .map(Delivered::Inserted),
        );
    } else {
        return;
    }
    delivered.push(Delivered::Panicked);
    events.extend(delivered.into_iter().map(Event::Delivered));
}

const fn reopened(recognized: Recognized<Abstract>) -> Delivered<Abstract> {
    Delivered::Reopened(Ok(Reopened {
        footage: (),
        checkpoint: (),
        pending: (),
        recognized,
    }))
}

fn published() -> [Delivered<Abstract>; 5] {
    [
        Published::Saved {
            shown: (),
            saved: (),
            copied: Ok(()),
        },
        Published::Saved {
            shown: (),
            saved: (),
            copied: Err(()),
        },
        Published::Recovered {
            shown: (),
            recovered: (),
        },
        Published::Cancelled,
        Published::Failed(()),
    ]
    .map(Delivered::Published)
}

fn completions(events: &mut Events, machine: &Machine<Abstract>) {
    match machine.worker {
        Worker::Loading => {
            events.extend(
                [Completion::Loaded(Ok(())), Completion::Loaded(Err(()))].map(Event::Worker),
            );
        },
        Worker::Transcribing(ticket) => {
            let words = ticket.part == Part::Final
                && machine
                    .session
                    .as_ref()
                    .is_some_and(|session| session.id == ticket.session && session.is_show());
            let transcript = if words {
                Transcript::Words(())
            } else {
                Transcript::Text(())
            };
            events.push(Event::Worker(Completion::Transcribed(
                ticket,
                Ok(transcript),
            )));
            events.push(Event::Worker(Completion::Transcribed(ticket, Err(()))));
            if ticket.part == Part::Stream {
                events.push(Event::Worker(Completion::Abandoned(ticket)));
            }
        },
        Worker::Unavailable | Worker::Ready => {},
    }
}

fn abandoned(events: &mut Events, id: SessionId) {
    let delivered = [
        Delivered::Abandoned(Ok(())),
        Delivered::Abandoned(Err(())),
        Delivered::Inserted(Inserted::Sent),
        Delivered::Reopened(Err(())),
        Delivered::Panicked,
    ];
    events.extend(
        delivered
            .into_iter()
            .chain(published())
            .map(|delivered| Event::Abandoned(id, delivered)),
    );
}
