use super::*;

fn mac_repeat(code: i64, held: Modifiers) -> MacEvent {
    MacEvent::Key {
        code,
        down: true,
        repeat: true,
        held,
    }
}

fn mac_modifier(code: i64, talk: bool) -> MacEvent {
    MacEvent::FlagsChanged {
        code,
        held: Modifiers {
            talk,
            ..NO_MODIFIERS
        },
    }
}

#[test]
fn a_key_already_down_when_a_hold_starts_never_acts_on_its_repeats() {
    let start = now();
    let later = start + APPLICATION_SHORTCUT;
    let actions = [
        (MAC_S, LETTER_S, Action::Show(Scope::Screen)),
        (MAC_V, LETTER_V, Action::PasteLast),
        (MAC_SPACE, SPACE, Action::Lock),
    ];
    let talking = Modifiers {
        talk: true,
        ..NO_MODIFIERS
    };
    for (shortcut, talk) in [
        (MacShortcut::FUNCTION, MAC_FN),
        (MacShortcut::RIGHT_OPTION, MAC_RIGHT_OPTION),
    ] {
        for (code, _, action) in actions {
            let mut mac = Mac::new(only(shortcut));
            assert!(
                !mac.observe(key(code, true, NO_MODIFIERS), keyboard_fn_up, start)
                    .swallows()
            );
            assert!(
                mac.observe(mac_modifier(talk, true), keyboard_fn_up, start)
                    .starts()
            );
            let repeat = mac.observe(mac_repeat(code, talking), keyboard_fn_up, later);
            assert_eq!(
                delivered(&repeat),
                [],
                "A key held before the hold acted on its repeat"
            );
            assert!(!repeat.swallows());
            mac.observe(key(code, false, talking), keyboard_fn_up, later);
            let press = mac.observe(key(code, true, talking), keyboard_fn_up, later);
            assert_eq!(delivered(&press), [action]);
            assert!(press.swallows());
        }
    }

    for shortcut in [WindowsShortcut::CONTROL_WINDOWS, WindowsShortcut::F13] {
        for (_, vk, action) in actions {
            let mut policy = Windows::new(only(shortcut));
            assert!(!policy.observe(vk, true, NONE_HELD, start).swallows());
            let chord = if shortcut == WindowsShortcut::F13 {
                assert!(policy.observe(F13, true, NONE_HELD, start).starts());
                held_for(shortcut, &[])
            } else {
                policy.observe(LEFT_CONTROL, true, NONE_HELD, start);
                assert!(
                    policy
                        .observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), start)
                        .starts()
                );
                held(&[LEFT_CONTROL, LEFT_WINDOWS])
            };
            let repeat = policy.observe(vk, true, chord, start);
            assert_eq!(
                delivered(&repeat),
                [],
                "A key held before the hold acted on its repeat"
            );
            assert!(!repeat.swallows());
            policy.observe(vk, false, chord, start);
            let press = policy.observe(vk, true, chord, start);
            assert_eq!(delivered(&press), [action]);
            assert!(press.swallows());
            let repeat = policy.observe(vk, true, chord, start);
            assert_eq!(
                delivered(&repeat),
                [],
                "A held key acted again on its repeat"
            );
            assert!(repeat.swallows());
        }
    }
}

#[test]
fn a_key_held_before_a_hold_repeats_through_it_without_interrupting() {
    // A remapper turns a held Caps Lock into F13 after the hook sees Caps Lock and its repeats.
    const CAPS_LOCK: u32 = 0x14;
    let start = now();
    let mut policy = Windows::new(only(WindowsShortcut::F13));
    let f13 = held_for(WindowsShortcut::F13, &[]);
    assert_eq!(
        delivered(&policy.observe(CAPS_LOCK, true, NONE_HELD, start)),
        []
    );
    assert!(policy.observe(F13, true, NONE_HELD, start).starts());
    let repeat = policy.observe(CAPS_LOCK, true, f13, start);
    assert_eq!(
        delivered(&repeat),
        [],
        "A held key's repeat interrupted the hold"
    );
    assert!(!repeat.swallows());
    assert_eq!(
        delivered(&policy.observe(F13, false, f13, start)),
        [Action::Release]
    );

    let talking = Modifiers {
        talk: true,
        ..NO_MODIFIERS
    };
    let mut mac = Mac::default();
    mac.observe(key(MAC_Q, true, NO_MODIFIERS), keyboard_fn_up, start);
    assert!(mac.observe(function(true), keyboard_fn_up, start).starts());
    let repeat = mac.observe(mac_repeat(MAC_Q, talking), keyboard_fn_up, start);
    assert_eq!(
        delivered(&repeat),
        [],
        "A held key's repeat interrupted the hold"
    );
    assert_eq!(
        delivered(&mac.observe(function(false), keyboard_fn_up, start)),
        [Action::Release]
    );
}

#[test]
fn a_swallowed_key_stays_swallowed_when_the_alternative_shortcut_is_released_first() {
    let start = now();
    let later = start + APPLICATION_SHORTCUT;
    let talking = Modifiers {
        talk: true,
        ..NO_MODIFIERS
    };
    for (shortcut, code) in [
        (MacShortcut::RIGHT_OPTION, MAC_RIGHT_OPTION),
        (MacShortcut::RIGHT_COMMAND, MAC_RIGHT_COMMAND),
    ] {
        let mut mac = Mac::new(only(shortcut));
        assert!(
            mac.observe(mac_modifier(code, true), keyboard_fn_up, start)
                .starts()
        );
        let submit = mac.observe(key(MAC_RETURN, true, talking), keyboard_fn_up, later);
        assert_eq!(delivered(&submit), [Action::Submit]);
        assert!(submit.swallows());
        assert_eq!(
            delivered(&mac.observe(mac_modifier(code, false), keyboard_fn_up, later)),
            [Action::Release]
        );
        let repeat = mac.observe(mac_repeat(MAC_RETURN, NO_MODIFIERS), keyboard_fn_up, later);
        assert!(
            repeat.swallows(),
            "Return repeated into the app once the shortcut was released"
        );
        assert_eq!(delivered(&repeat), []);
        assert!(
            mac.observe(key(MAC_RETURN, false, NO_MODIFIERS), keyboard_fn_up, later)
                .swallows()
        );
        assert!(
            !mac.observe(key(MAC_RETURN, true, NO_MODIFIERS), keyboard_fn_up, later)
                .swallows()
        );
    }

    let right_control = held_for(WindowsShortcut::RIGHT_CONTROL, &[RIGHT_CONTROL]);
    let mut policy = Windows::new(only(WindowsShortcut::RIGHT_CONTROL));
    assert!(
        policy
            .observe(RIGHT_CONTROL, true, NONE_HELD, start)
            .starts()
    );
    assert_eq!(
        delivered(&policy.observe(RETURN, true, right_control, later)),
        [Action::Submit]
    );
    assert_eq!(
        delivered(&policy.observe(RIGHT_CONTROL, false, right_control, later)),
        [Action::Release]
    );
    let again = later + APPLICATION_SHORTCUT;
    assert!(
        policy
            .observe(RIGHT_CONTROL, true, NONE_HELD, again)
            .starts()
    );
    let repeat = policy.observe(RETURN, true, right_control, again);
    assert!(
        repeat.swallows(),
        "Return repeated into the app as a quick Right Ctrl shortcut"
    );
    assert_eq!(delivered(&repeat), []);
}

#[test]
fn windows_swallows_a_held_keys_repeats_until_its_release() {
    let (mut policy, both) = windows_holding();
    assert_eq!(
        delivered(&policy.observe(SPACE, true, both, now())),
        [Action::Lock]
    );
    assert_eq!(
        delivered(&policy.observe(LEFT_WINDOWS, false, both, now())),
        [Action::Release]
    );
    policy.observe(LETTER_Q, true, NONE_HELD, now());
    policy.observe(LETTER_Q, false, NONE_HELD, now());
    assert!(
        policy.observe(SPACE, true, NONE_HELD, now()).swallows(),
        "A held Space repeated into the editor"
    );
    assert!(policy.observe(SPACE, false, NONE_HELD, now()).swallows());
    assert!(
        !policy.observe(SPACE, true, NONE_HELD, now()).swallows(),
        "A fresh Space after its release was swallowed"
    );
}

#[test]
fn mac_takes_autorepeat_from_the_event_for_hold_keys() {
    let mut mac = mac_holding();
    assert_eq!(
        delivered(&mac.observe(key(MAC_SPACE, true, FN_HELD), keyboard_fn_up, now())),
        [Action::Lock]
    );
    assert_eq!(
        delivered(&mac.observe(function(false), keyboard_fn_up, now())),
        [Action::Release]
    );
    let repeat = MacEvent::Key {
        code: MAC_SPACE,
        down: true,
        repeat: true,
        held: NO_MODIFIERS,
    };
    assert!(
        mac.observe(repeat, keyboard_fn_up, now()).swallows(),
        "A held Space repeated into the editor"
    );
    assert!(
        !mac.observe(key(MAC_SPACE, true, NO_MODIFIERS), keyboard_fn_up, now())
            .swallows(),
        "A fresh Space after its unseen release was swallowed"
    );
}

#[test]
fn a_held_hold_key_stays_swallowed_however_the_hold_ends_until_it_is_released_or_pressed_afresh() {
    type Ending = (&'static str, fn(&mut Talk));
    let ends: [Ending; 3] = [
        ("a release", |chord| assert!(chord.release().is_some())),
        ("a missed release", |chord| assert!(chord.reconcile(false))),
        ("an interrupt", |chord| assert!(chord.interrupt())),
    ];
    for key in [HoldKey::Space, HoldKey::Submit] {
        for (ending, end) in ends {
            let mut chord = Talk::default();
            assert_eq!(chord.press(), Some(Action::Press));
            assert!(chord.hold_key(key, Stroke::Press).swallows());
            assert!(
                chord.hold_key(key, Stroke::Repeat).swallows(),
                "A held shortcut must swallow repeats"
            );
            end(&mut chord);
            assert!(
                chord.hold_key(key, Stroke::Repeat).swallows(),
                "After {ending}, a held key repeated into the editor"
            );
            assert!(
                !chord.hold_key(key, Stroke::Press).swallows(),
                "After {ending}, a fresh press whose release went unseen was swallowed"
            );
            assert!(!chord.hold_key(key, Stroke::Release).swallows());
        }
    }
}
