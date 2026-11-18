use super::*;

const LETTER_V: u32 = 0x56;
const TAB: u32 = 0x09;
const CAPS_LOCK: u32 = 0x14;
const F13: u32 = 0x7C;

fn types(policy: &mut Windows, key: u32, down: bool, physical: Physical) -> bool {
    let decision = policy.observe(key, down, physical, now());
    Windows::types(key, down, &decision)
}

#[test]
fn keys_that_reach_the_app_count_as_typing_and_modifiers_do_not() {
    let mut idle = Windows::default();
    for key in [LETTER_A, TAB, ESCAPE, SPACE, RETURN] {
        assert!(types(&mut idle, key, true, NONE_HELD), "{key:#x}");
        assert!(!types(&mut idle, key, false, NONE_HELD), "{key:#x} up");
    }
    for key in [SHIFT, LEFT_SHIFT, CONTROL, ALT, LEFT_WINDOWS, CAPS_LOCK] {
        assert!(!types(&mut idle, key, true, NONE_HELD), "{key:#x}");
        types(&mut idle, key, false, NONE_HELD);
    }
}

#[test]
fn the_shortcut_and_its_hold_keys_never_count_as_typing() {
    let mut policy = Windows::default();
    assert!(!types(&mut policy, LEFT_CONTROL, true, NONE_HELD));
    assert!(!types(
        &mut policy,
        LEFT_WINDOWS,
        true,
        held(&[LEFT_CONTROL])
    ));
    let both = held(&[LEFT_CONTROL, LEFT_WINDOWS]);
    for key in [LETTER_V, SPACE, RETURN, LETTER_S] {
        assert!(
            !types(&mut policy, key, true, both),
            "{key:#x} during a hold"
        );
    }

    let mut f13 = Windows::new(WindowsShortcut::F13);
    assert!(!types(
        &mut f13,
        F13,
        true,
        held_for(WindowsShortcut::F13, &[])
    ));
}

#[test]
fn mac_key_presses_that_reach_the_app_count_as_typing() {
    let mut idle = Mac::default();
    for code in [MAC_A, MAC_V, MAC_SPACE, MAC_RETURN, MAC_ESCAPE] {
        let down = idle.observe(key(code, true, NO_MODIFIERS), keyboard_fn_up, now());
        assert!(Mac::types(key(code, true, NO_MODIFIERS), &down), "{code}");
        let up = idle.observe(key(code, false, NO_MODIFIERS), keyboard_fn_up, now());
        assert!(
            !Mac::types(key(code, false, NO_MODIFIERS), &up),
            "{code} up"
        );
    }
    let flags = shift(true, NO_MODIFIERS);
    let decision = idle.observe(flags, keyboard_fn_up, now());
    assert!(!Mac::types(flags, &decision));
}

#[test]
fn mac_hold_keys_and_the_f13_shortcut_never_count_as_typing() {
    let mut holding = mac_holding();
    for code in [MAC_V, MAC_S, MAC_RETURN] {
        let event = key(code, true, FN_HELD);
        let decision = holding.observe(event, keyboard_fn_up, now());
        assert!(!Mac::types(event, &decision), "{code} during a hold");
    }

    let mut f13 = Mac::new(MacShortcut::F13);
    for repeat in [false, true] {
        let event = MacEvent::Key {
            code: MAC_F13,
            down: true,
            repeat,
            held: NO_MODIFIERS,
        };
        let decision = f13.observe(event, || true, now());
        assert!(!Mac::types(event, &decision), "F13 repeat: {repeat}");
    }
}
