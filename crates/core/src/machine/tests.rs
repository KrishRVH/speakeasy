use super::*;
use crate::machine::fixture::{Log, Sample};

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
fn accepting_a_preset_clears_an_old_failure() {
    let mut log = Log::default();
    let mut machine = machine(&mut log);
    let failed = Event::Worker(Completion::Loaded(Err("previous model failed".into())));
    assert!(machine.step(now(), failed, &mut log).is_continue());
    assert_eq!(machine.phase(), Phase::Error);
    let preset = Event::Preset {
        call: (),
        name: "Another preset".into(),
        config: settings(),
    };
    assert!(machine.step(now(), preset, &mut log).is_continue());
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
