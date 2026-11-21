use super::*;
use crate::machine::fixture::{Log, Sample};

fn saved(shown: &'static str) -> Delivered<Sample> {
    Delivered::Published(Published::Saved {
        shown,
        saved: (),
        copied: Ok(()),
    })
}

#[test]
fn a_show_saved_after_its_session_was_abandoned_never_replaces_a_later_sessions() {
    let first = SessionId::FIRST;
    let second = first.next().unwrap();
    assert_eq!(saved_anyway(None, first, saved("first")), Some("first"));
    assert_eq!(
        saved_anyway(Some(first), second, saved("second")),
        Some("second")
    );
    assert_eq!(
        saved_anyway(Some(second), first, saved("first")),
        None,
        "An earlier session's Show replaced a later one"
    );
}

fn now() -> Now<Sample> {
    Now {
        moment: Moment::default(),
        instant: std::time::Duration::ZERO,
    }
}

const fn settings() -> Settings {
    Settings {
        play_sounds: false,
        recognition: RecognitionMode::Whole,
        probe: false,
    }
}

fn machine(log: &mut Log) -> Machine<Sample> {
    Machine::new(
        Timing::DICTATION,
        settings(),
        true,
        now().instant,
        None,
        log,
    )
}

#[test]
fn explicit_show_needs_a_shield_before_opening_the_microphone() {
    let mut log = Log::default();
    log.refuse_shield = true;
    let mut machine = machine(&mut log);
    assert!(
        machine
            .step(now(), Event::Input(Input::StartShow(())), &mut log)
            .is_continue()
    );
    assert!(machine.is_idle());
    assert!(!log.microphone_opened);
    assert!(!machine.hands_free());
}

#[test]
fn accepting_a_preset_clears_an_old_failure() {
    let mut log = Log::default();
    let mut machine = machine(&mut log);
    for event in [
        Event::Worker(Completion::Loaded(Err("previous model failed".into()))),
        Event::Preset {
            call: (),
            name: "Another preset".into(),
            config: settings(),
            session: SessionId::default(),
            changed: false,
        },
        Event::Worker(Completion::Loaded(Ok(()))),
    ] {
        assert!(machine.step(now(), event, &mut log).is_continue());
    }
    assert_eq!(machine.phase(), Phase::Idle);
}

#[test]
fn promotion_leaves_a_private_script_capture_as_dictation() {
    let mut log = Log::default();
    let mut machine = machine(&mut log);
    for event in [
        Event::Worker(Completion::Loaded(Ok(()))),
        Event::Control {
            call: (),
            command: Command::Capture(None),
        },
        Event::Input(Input::Show(())),
    ] {
        assert!(machine.step(now(), event, &mut log).is_continue());
    }
    assert!(machine.for_script());
    assert!(!machine.shows());
}

#[test]
fn a_show_keeps_its_microphone_loss_notice_while_the_worker_reloads_and_retries() {
    let mut log = Log::default();
    let mut machine = machine(&mut log);
    assert!(
        machine
            .step(now(), Event::Input(Input::StartShow(())), &mut log)
            .is_continue()
    );
    let id = machine.latest.id;
    assert!(
        machine
            .step(now(), Event::Capture(id, Capture::Ready), &mut log)
            .is_continue()
    );

    machine.worker = Worker::Unavailable;
    let lost = Capture::Lost(
        Sealed {
            audio: vec![1],
            trimmed: std::time::Duration::ZERO,
            partition: None,
        },
        Lost {
            notice: "Microphone disconnected".into(),
            failure: "Reconnect the microphone".into(),
        },
    );
    let loss_remains = |machine: &Machine<Sample>| {
        assert!(
            matches!(machine.notice(), Some(Notice::NarrationLost(text)) if text == "Microphone disconnected")
        );
    };
    assert!(
        machine
            .step(now(), Event::Capture(id, lost), &mut log)
            .is_continue()
    );
    loss_remains(&machine);
    assert!(
        machine
            .step(now(), Event::Worker(Completion::Loaded(Ok(()))), &mut log)
            .is_continue()
    );
    let Worker::Transcribing(ticket) = machine.worker else {
        panic!("The worker did not start transcribing the partial narration");
    };
    assert!(
        machine
            .step(
                now(),
                Event::Worker(Completion::Transcribed(
                    ticket,
                    Err("Transcription stopped".into())
                )),
                &mut log
            )
            .is_continue()
    );
    loss_remains(&machine);
}
