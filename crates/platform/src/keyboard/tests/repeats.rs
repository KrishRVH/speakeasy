use super::*;

const LETTER_V: u32 = 0x56;

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
        (MacShortcut::Function, MAC_FN),
        (MacShortcut::RightOption, MAC_RIGHT_OPTION),
    ] {
        for (code, _, action) in actions {
            let mut mac = Mac::new(shortcut);
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

    for shortcut in [WindowsShortcut::ControlWindows, WindowsShortcut::F13] {
        for (_, vk, action) in actions {
            let mut policy = Windows::new(shortcut);
            assert!(!policy.observe(vk, true, NONE_HELD, start).swallows());
            let chord = if shortcut == WindowsShortcut::F13 {
                assert!(policy.observe(0x7C, true, NONE_HELD, start).starts());
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
fn windows_takes_a_key_down_as_a_repeat_until_the_hook_sees_its_release() {
    let start = now();
    let at = |millis| start + Duration::from_millis(millis);
    let (mut policy, both) = windows_holding();
    assert_eq!(
        delivered(&policy.observe(SPACE, true, both, at(0))),
        [Action::Lock]
    );
    assert_eq!(
        delivered(&policy.observe(LEFT_WINDOWS, false, both, at(100))),
        [Action::Release]
    );
    for millis in [1_000, 3_000, 30_000] {
        assert!(
            policy
                .observe(SPACE, true, NONE_HELD, at(millis))
                .swallows(),
            "A held Space repeated into the editor at {millis} ms"
        );
    }
    policy.observe(LETTER_Q, true, NONE_HELD, at(30_100));
    policy.observe(LETTER_Q, false, NONE_HELD, at(30_150));
    assert!(
        policy
            .observe(SPACE, true, NONE_HELD, at(30_200))
            .swallows(),
        "A Space held beside other typing repeated into the editor"
    );
    assert!(
        policy
            .observe(SPACE, false, NONE_HELD, at(30_300))
            .swallows()
    );
    assert!(
        !policy
            .observe(SPACE, true, NONE_HELD, at(30_400))
            .swallows(),
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
fn a_windows_lifecycle_interrupt_forgets_the_keys_held_before_it() {
    let (sender, _events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let (mut policy, both) = windows_holding();
    assert!(policy.observe(SPACE, true, both, now()).swallows());
    policy.interrupt(&input);
    assert!(
        !policy.observe(SPACE, true, both, now()).swallows(),
        "A Space pressed after the lock screen was swallowed"
    );
}

#[test]
fn a_held_hold_key_stays_swallowed_however_the_hold_ends_until_it_is_released_or_pressed_afresh() {
    type Ending = (&'static str, fn(&mut Chord));
    let ends: [Ending; 3] = [
        ("a release", |chord| assert!(chord.release().is_some())),
        ("a missed release", |chord| assert!(chord.reconcile(false))),
        ("an interrupt", |chord| assert!(chord.interrupt())),
    ];
    for key in [HoldKey::Space, HoldKey::Submit] {
        for (ending, end) in ends {
            let mut chord = Chord::default();
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
