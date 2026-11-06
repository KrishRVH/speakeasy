use super::*;
use crate::MacShortcut;

#[test]
fn f13_autorepeat_preserves_shift_taps_during_a_show() {
    let start = now();
    let at = |millis| start + Duration::from_millis(millis);
    for repeats in [false, true] {
        let mut policy = Mac::new(MacShortcut::F13);
        policy.observe(f13(true, false, NO_MODIFIERS), || true, at(0));
        for (millis, down) in [(100, true), (180, false), (300, true)] {
            policy.observe(shift(down, NO_MODIFIERS), || true, at(millis));
            if repeats {
                assert_eq!(
                    delivered(&policy.observe(
                        f13(true, true, NO_MODIFIERS),
                        || true,
                        at(millis + 20)
                    )),
                    []
                );
            }
        }
        let pinned = policy.observe(shift(false, NO_MODIFIERS), || true, at(380));
        assert_eq!(
            delivered(&pinned),
            [Action::Pin(Taps {
                pressed: at(100),
                released: at(380)
            })]
        );
        assert!(!pinned.swallows());
    }
}

fn f13(down: bool, repeat: bool, held: Modifiers) -> MacEvent {
    MacEvent::Key {
        code: 105,
        down,
        repeat,
        held,
    }
}

#[test]
fn a_dedicated_f13_key_holds_repeats_and_releases_without_leaking_to_the_editor() {
    for flags in [0, 0x0080_0000] {
        let held = MacShortcut::F13.modifiers(flags);
        let mut policy = Mac::new(MacShortcut::F13);
        let press = policy.observe(f13(true, false, held), || true, now());
        assert_eq!(delivered(&press), [Action::Press]);
        assert!(press.swallows());
        let repeat = policy.observe(f13(true, true, held), || true, now());
        assert_eq!(delivered(&repeat), []);
        assert!(repeat.swallows());
        let release = policy.observe(f13(false, false, NO_MODIFIERS), || false, now());
        assert_eq!(delivered(&release), [Action::Release]);
        assert!(release.swallows());
        let press = policy.observe(f13(true, false, held), || true, now());
        assert_eq!(delivered(&press), [Action::Press]);
    }
}

#[test]
fn f13_hold_commands_are_immediate_and_escape_stays_passive() {
    for (code, action) in [
        (MAC_SPACE, Action::Lock),
        (MAC_S, Action::Show(Scope::Screen)),
        (MAC_W, Action::Show(Scope::Window)),
        (MAC_A, Action::Show(Scope::Area)),
        (9, Action::PasteLast),
    ] {
        let mut policy = Mac::new(MacShortcut::F13);
        let at = now();
        policy.observe(f13(true, false, NO_MODIFIERS), || true, at);
        let command = policy.observe(key(code, true, NO_MODIFIERS), || true, at);
        assert_eq!(delivered(&command), [action]);
        assert!(command.swallows());
        let escape = policy.observe(key(MAC_ESCAPE, true, NO_MODIFIERS), || true, at);
        assert_eq!(delivered(&escape), [Action::Escape]);
        assert!(!escape.swallows());
        policy.observe(f13(false, false, NO_MODIFIERS), || false, at);
        assert!(
            policy
                .observe(key(code, false, NO_MODIFIERS), || false, at)
                .swallows()
        );
        assert!(
            !policy
                .observe(key(code, true, NO_MODIFIERS), || false, at)
                .swallows()
        );
    }
}

#[test]
fn f13_repeats_do_not_restart_after_interruption_and_a_fresh_press_recovers_a_lost_release() {
    let mut policy = Mac::new(MacShortcut::F13);
    assert_eq!(
        delivered(&policy.observe(f13(true, true, NO_MODIFIERS), || true, now())),
        []
    );
    policy.observe(f13(true, false, NO_MODIFIERS), || true, now());
    let interrupt = policy.observe(key(MAC_Q, true, NO_MODIFIERS), || true, now());
    assert_eq!(delivered(&interrupt), [Action::Interrupt]);
    assert!(!interrupt.swallows());
    assert_eq!(
        delivered(&policy.observe(f13(true, true, NO_MODIFIERS), || true, now())),
        []
    );
    assert!(
        !policy
            .observe(key(MAC_S, true, NO_MODIFIERS), || true, now())
            .swallows()
    );
    assert_eq!(
        delivered(&policy.observe(f13(true, false, NO_MODIFIERS), || true, now())),
        [Action::Press]
    );
    assert_eq!(
        delivered(&policy.observe(f13(true, false, NO_MODIFIERS), || true, now())),
        [Action::Release, Action::Press]
    );
    let stale = policy.observe(key(MAC_S, true, NO_MODIFIERS), || false, now());
    assert_eq!(delivered(&stale), [Action::Release]);
    assert!(!stale.swallows());
}

#[test]
fn f13_requires_other_shortcuts_to_be_released_and_unselected_f13_passes_through() {
    for flags in [0x0004_0000, 0x0008_0020, 0x0010_0008] {
        let mut policy = Mac::new(MacShortcut::F13);
        let held = MacShortcut::F13.modifiers(flags);
        assert_eq!(
            delivered(&policy.observe(f13(true, false, held), || true, now())),
            []
        );
        assert_eq!(
            delivered(&policy.observe(f13(true, true, NO_MODIFIERS), || true, now())),
            []
        );
        assert_eq!(
            delivered(&policy.observe(f13(false, false, NO_MODIFIERS), || false, now())),
            []
        );
        assert_eq!(
            delivered(&policy.observe(f13(true, false, NO_MODIFIERS), || true, now())),
            [Action::Press]
        );
    }
    for shortcut in [
        MacShortcut::Function,
        MacShortcut::RightOption,
        MacShortcut::RightCommand,
    ] {
        let mut policy = Mac::new(shortcut);
        for down in [true, false] {
            let decision = policy.observe(f13(down, false, NO_MODIFIERS), || false, now());
            assert_eq!(delivered(&decision), []);
            assert!(!decision.swallows());
        }
    }
}
