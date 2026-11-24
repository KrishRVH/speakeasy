use super::*;

const LETTER_H: u32 = 0x48;
const LEFT_ALT: u32 = 0xA4;
const MAC_H: i64 = 4;
const MAC_F5: i64 = 96;
const MAC_FUNCTION_FLAG: u64 = 0x0080_0000;
const MAC_CONTROL_FLAGS: u64 = 0x0004_0000 | 0x01;
const MAC_OPTION_FLAGS: u64 = 0x0008_0000 | 0x20;

fn windows(talk: &str, toggles: Toggles<WindowsShortcut>) -> Windows {
    Windows::new(Shortcuts {
        talk: talk.parse().unwrap_or_default(),
        toggles,
    })
}

fn mac(talk: &str, toggles: Toggles<MacShortcut>) -> Mac {
    Mac::new(Shortcuts {
        talk: talk.parse().unwrap_or_default(),
        toggles,
    })
}

fn no_toggles<S>() -> Toggles<S> {
    Toggles {
        hands_free: None,
        show_screen: None,
        show_window: None,
        show_area: None,
    }
}

#[test]
fn a_recorded_shortcut_with_a_key_claims_only_the_press_that_completes_it() {
    let talk = "control+shift+space";
    let mut policy = windows(talk, no_toggles());
    let read = |keys: &[u32]| held_for(talk.parse().unwrap_or_default(), keys);
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    policy.observe(LEFT_SHIFT, true, read(&[LEFT_CONTROL]), now());
    let both = read(&[LEFT_CONTROL, LEFT_SHIFT]);
    let start = policy.observe(SPACE, true, both, now());
    assert!(start.swallows(), "The completing key reached the app");
    assert!(start.starts());
    assert!(
        policy.observe(SPACE, true, both, now()).swallows(),
        "A repeat reached the app"
    );
    assert_eq!(
        delivered(&policy.observe(LEFT_SHIFT, false, both, now())),
        [Action::Release],
        "Lifting a modifier did not end the hold"
    );
    let release = policy.observe(SPACE, false, read(&[LEFT_CONTROL]), now());
    assert!(
        release.swallows(),
        "The claimed key's release reached the app"
    );
    assert_eq!(delivered(&release), []);

    let typed = policy.observe(SPACE, true, read(&[LEFT_CONTROL]), now());
    assert!(
        !typed.swallows(),
        "Space without the whole shortcut was swallowed"
    );
    assert!(!typed.starts());
}

#[test]
fn a_shortcut_of_several_modifiers_starts_once_all_are_held_and_ends_when_one_lifts() {
    let mut policy = mac("control+option", no_toggles());
    let flags = |flags| MacShortcut::modifiers("control+option".parse().unwrap_or_default(), flags);
    let control = MacEvent::FlagsChanged {
        code: 59,
        held: flags(MAC_CONTROL_FLAGS),
    };
    assert_eq!(
        delivered(&policy.observe(control, keyboard_fn_up, now())),
        []
    );
    let option = MacEvent::FlagsChanged {
        code: 58,
        held: flags(MAC_CONTROL_FLAGS | MAC_OPTION_FLAGS),
    };
    assert!(policy.observe(option, keyboard_fn_up, now()).starts());
    let lifted = MacEvent::FlagsChanged {
        code: 59,
        held: flags(MAC_OPTION_FLAGS),
    };
    assert_eq!(
        delivered(&policy.observe(lifted, keyboard_fn_up, now())),
        [Action::Release]
    );
}

#[test]
fn a_toggle_acts_once_on_its_exact_keys_and_its_key_never_reaches_the_app() {
    let toggles = Toggles {
        hands_free: "control+alt+h".parse().ok(),
        ..no_toggles()
    };
    let mut policy = windows("f13", toggles);
    let read = |keys: &[u32]| held_for(WindowsShortcut::F13, keys);
    let chord = read(&[LEFT_CONTROL, LEFT_ALT]);
    let press = policy.observe(LETTER_H, true, chord, now());
    assert!(press.swallows());
    assert_eq!(delivered(&press), [Action::Toggle(Toggle::HandsFree)]);
    let repeat = policy.observe(LETTER_H, true, chord, now());
    assert!(repeat.swallows());
    assert_eq!(delivered(&repeat), [], "A repeat toggled again");
    assert!(policy.observe(LETTER_H, false, chord, now()).swallows());

    let extra = policy.observe(
        LETTER_H,
        true,
        read(&[LEFT_CONTROL, LEFT_ALT, LEFT_SHIFT]),
        now(),
    );
    assert!(!extra.swallows(), "Extra modifiers matched a toggle");
    policy.observe(LETTER_H, false, NONE_HELD, now());
    assert!(
        !policy.observe(LETTER_H, true, NONE_HELD, now()).swallows(),
        "H alone was swallowed"
    );
}

#[test]
fn a_mac_toggle_on_a_function_key_matches_despite_the_fn_flag_macos_adds() {
    let toggles = Toggles {
        show_screen: "control+f5".parse().ok(),
        ..no_toggles()
    };
    let mut policy = mac("function", toggles);
    let held = MacShortcut::FUNCTION.modifiers(MAC_CONTROL_FLAGS | MAC_FUNCTION_FLAG);
    let press = policy.observe(key(MAC_F5, true, held), keyboard_fn_up, now());
    assert_eq!(
        delivered(&press),
        [Action::Toggle(Toggle::Show(Scope::Screen))]
    );
    assert!(press.swallows());
    let plain = MacShortcut::FUNCTION.modifiers(MAC_FUNCTION_FLAG);
    let other = policy.observe(key(MAC_H, true, plain), keyboard_fn_up, now());
    assert!(!other.swallows());
}

#[test]
fn a_mac_toggle_with_fn_fires_beside_a_talk_shortcut_that_has_a_key() {
    let toggles = Toggles {
        hands_free: "function+h".parse().ok(),
        ..no_toggles()
    };
    let mut policy = mac("f13", toggles);
    let held = MacShortcut::F13.modifiers(MAC_FUNCTION_FLAG);
    let press = policy.observe(key(MAC_H, true, held), keyboard_fn_up, now());
    assert_eq!(delivered(&press), [Action::Toggle(Toggle::HandsFree)]);
}

#[test]
fn a_key_released_while_a_recording_takes_it_presses_fresh_afterwards() {
    let mut policy = Windows::new(only(WindowsShortcut::CONTROL_WINDOWS));
    policy.observe(SPACE, true, NONE_HELD, now());
    policy.track(SPACE, false);
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    let control = held_for(WindowsShortcut::CONTROL_WINDOWS, &[LEFT_CONTROL]);
    assert!(policy.observe(LEFT_WINDOWS, true, control, now()).starts());
    let both = held_for(
        WindowsShortcut::CONTROL_WINDOWS,
        &[LEFT_CONTROL, LEFT_WINDOWS],
    );
    assert_eq!(
        delivered(&policy.observe(SPACE, true, both, now())),
        [Action::Lock],
        "Space read as a repeat of a press released during the recording"
    );
}

#[test]
fn the_recorder_reports_held_modifiers_and_ends_on_a_key_a_release_or_escape() {
    let right_option = Pressed::NONE.with(Modifier::Alt, Side::Right, true);
    let mut recorder = Recorder::default();
    assert!(matches!(
        recorder.modifiers(right_option),
        Recorded::Holding(Some(_))
    ));
    assert_eq!(
        recorder.modifiers(Pressed::NONE),
        Recorded::Chosen(NativeShortcut::new(
            Chord::recorded(
                ModifierSet::EMPTY.with(Modifier::Alt),
                Some((Modifier::Alt, Side::Right)),
                None,
            )
            .expect("Option was held"),
        )),
        "Releasing a lone modifier did not record it with its side"
    );

    let mut recorder = Recorder::default();
    let control = Pressed::NONE.with(Modifier::Control, Side::Left, true);
    recorder.modifiers(control);
    let both = control.with(Modifier::Super, Side::Right, true);
    recorder.modifiers(both);
    recorder.modifiers(control);
    assert!(
        matches!(recorder.modifiers(Pressed::NONE), Recorded::Chosen(shortcut) if !shortcut.has_key()),
        "Modifiers held together were not recorded together"
    );

    let mut recorder = Recorder::default();
    recorder.modifiers(control);
    assert!(
        matches!(recorder.key(0x48, false, None), Recorded::Chosen(shortcut) if shortcut.has_key())
    );
    assert_eq!(
        Recorder::default().key(0x1B, true, None),
        Recorded::Cancelled
    );
    assert!(
        matches!(recorder.key(0x1B, true, None), Recorded::Chosen(_)),
        "Esc with a modifier is a recording for Settings to refuse"
    );
}

#[test]
fn a_mac_recording_leaves_out_the_fn_flag_macos_sets_on_function_keys() {
    let function = Pressed::NONE.with(Modifier::Function, Side::Left, true);
    let mut recorder = Recorder::default();
    recorder.modifiers(function);
    let f5 = u16::try_from(MAC_F5).unwrap_or_default();
    let Recorded::Chosen(shortcut) = recorder.key(f5, false, Some(Modifier::Function)) else {
        panic!("F5 did not end the recording");
    };
    assert_eq!(MacShortcut::new(shortcut.chord()).to_string(), "f5");
    assert!(matches!(
        Mac::recorded(key(MAC_F5, true, NO_MODIFIERS), 0),
        RecordedKey::Key {
            implied: Some(Modifier::Function),
            ..
        }
    ));
    assert!(matches!(
        Mac::recorded(key(MAC_H, true, NO_MODIFIERS), 0),
        RecordedKey::Key { implied: None, .. }
    ));
}

#[test]
fn a_recording_takes_keys_from_the_shortcuts_until_it_ends_and_passes_keys_held_before_it() {
    let recording = Recording::default();
    assert_eq!(
        recording.observe(RecordedKey::Ignored),
        None,
        "No recording runs yet"
    );
    let results = recording.start();
    let control = Pressed::NONE.with(Modifier::Control, Side::Left, true);
    assert_eq!(
        recording.observe(RecordedKey::Modifiers(control)),
        Some(false),
        "A recording swallowed a modifier"
    );
    let stroke = |code, stroke| RecordedKey::Key {
        code,
        escape: false,
        stroke,
        implied: None,
    };
    assert_eq!(
        recording.observe(stroke(0x41, Stroke::Release)),
        Some(false),
        "A key held from before the recording was swallowed"
    );
    let press = stroke(0x4B, Stroke::Press);
    assert_eq!(recording.observe(press), Some(true));
    assert!(matches!(results.try_recv(), Ok(Recorded::Holding(Some(_)))));
    assert!(matches!(results.try_recv(), Ok(Recorded::Chosen(shortcut)) if shortcut.has_key()));
    assert!(results.is_closed(), "The recording outlived its result");
    assert_eq!(
        recording.observe(stroke(0x4B, Stroke::Repeat)),
        Some(true),
        "A repeat of the recorded key reached the app"
    );
    assert_eq!(
        recording.observe(stroke(0x41, Stroke::Press)),
        None,
        "Another key waited on the recorded key's release"
    );
    assert_eq!(
        recording.observe(stroke(0x4B, Stroke::Release)),
        Some(true),
        "The recorded key's release reached the app without its press"
    );
    assert_eq!(
        recording.observe(press),
        None,
        "Keys went to a finished recording"
    );

    drop(recording.start());
    assert_eq!(
        recording.observe(press),
        None,
        "A recording nobody listens to swallowed a key"
    );
}

#[test]
fn recorded_windows_keys_include_the_event_itself_and_tell_repeats_from_presses() {
    let control = |key: u16| u32::from(key) == LEFT_CONTROL;
    assert!(matches!(
        Windows::recorded(LEFT_ALT, true, control),
        RecordedKey::Modifiers(pressed)
            if pressed.has(Modifier::Control, Some(Side::Left))
                && pressed.has(Modifier::Alt, Some(Side::Left))
    ));
    assert!(matches!(
        Windows::recorded(LEFT_CONTROL, false, control),
        RecordedKey::Modifiers(pressed) if pressed.is_empty()
    ));
    assert!(matches!(
        Windows::recorded(LETTER_H, true, control),
        RecordedKey::Key {
            stroke: Stroke::Press,
            escape: false,
            ..
        }
    ));
    let h = |key: u16| u32::from(key) == LETTER_H;
    assert!(
        matches!(
            Windows::recorded(LETTER_H, true, h),
            RecordedKey::Key {
                stroke: Stroke::Repeat,
                ..
            }
        ),
        "A repeat read as a fresh press"
    );
    assert!(matches!(
        Windows::recorded(ESCAPE, true, |_| false),
        RecordedKey::Key { escape: true, .. }
    ));
}

#[test]
fn a_windows_shortcut_that_holds_win_or_alt_marks_it_used_so_start_stays_shut() {
    let mut policy = Windows::new(only(WindowsShortcut::CONTROL_WINDOWS));
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    let held = held_for(WindowsShortcut::CONTROL_WINDOWS, &[LEFT_CONTROL]);
    assert!(
        policy
            .observe(LEFT_WINDOWS, true, held, now())
            .uses_modifiers()
    );
    let control = |key: u16| u32::from(key) == LEFT_CONTROL;
    assert!(
        Windows::menu_held(LEFT_WINDOWS, true, control),
        "Win going down was not counted before async key state shows it"
    );
    assert!(!Windows::menu_held(RIGHT_CONTROL, true, |_| false));
    assert!(Windows::menu_held(LETTER_H, true, |key| u32::from(key) == LEFT_ALT));
}

#[test]
fn a_mac_shortcut_reads_as_held_only_when_each_of_its_keys_is_down() {
    let down = |keys: &'static [i64]| move |code: u16| keys.contains(&i64::from(code));
    let right_option = MacShortcut::RIGHT_OPTION.chord();
    assert!(Mac::chord_down(right_option, down(&[MAC_RIGHT_OPTION])));
    assert!(
        !Mac::chord_down(right_option, down(&[MAC_LEFT_OPTION])),
        "Left Option held a Right Option shortcut"
    );
    assert!(Mac::chord_down(
        MacShortcut::FUNCTION.chord(),
        down(&[MAC_FN])
    ));
    let keyed = "control+shift+space"
        .parse::<MacShortcut>()
        .expect("a valid shortcut")
        .chord();
    assert!(Mac::chord_down(
        keyed,
        down(&[MAC_LEFT_CONTROL, MAC_RIGHT_SHIFT, MAC_SPACE])
    ));
    assert!(!Mac::chord_down(
        keyed,
        down(&[MAC_LEFT_CONTROL, MAC_RIGHT_SHIFT])
    ));
}

#[test]
fn recorded_mac_keys_leave_out_the_globe_key_and_repeats() {
    let held = MacShortcut::FUNCTION.modifiers(MAC_CONTROL_FLAGS);
    let flags = MacEvent::FlagsChanged {
        code: MAC_LEFT_CONTROL,
        held,
    };
    assert!(matches!(
        Mac::recorded(flags, MAC_CONTROL_FLAGS),
        RecordedKey::Modifiers(pressed) if pressed.has(Modifier::Control, Some(Side::Left))
    ));
    assert!(
        matches!(
            Mac::recorded(key(MAC_GLOBE, true, held), 0),
            RecordedKey::Ignored
        ),
        "The Globe key recorded as a key of its own"
    );
    let repeat = MacEvent::Key {
        code: MAC_H,
        down: true,
        repeat: true,
        held,
    };
    assert!(matches!(
        Mac::recorded(repeat, 0),
        RecordedKey::Key {
            stroke: Stroke::Repeat,
            ..
        }
    ));
    assert!(matches!(
        Mac::recorded(key(MAC_ESCAPE, true, held), 0),
        RecordedKey::Key {
            escape: true,
            stroke: Stroke::Press,
            ..
        }
    ));
}
