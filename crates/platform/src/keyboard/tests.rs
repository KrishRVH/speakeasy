//! Drives the Ctrl+Win and Fn policies with synthetic key events and the held-modifier rules with
//! chosen key states; no test installs a hook or reads the keyboard.

use std::time::{Duration, Instant};

use anyhow::Context as _;

use super::*;

const LEFT_CONTROL: u32 = 0xA2;
const RIGHT_CONTROL: u32 = 0xA3;
const LEFT_WINDOWS: u32 = 0x5B;
const RIGHT_WINDOWS: u32 = 0x5C;
const ESCAPE: u32 = 0x1B;
const SPACE: u32 = 0x20;
const LETTER_S: u32 = 0x53;
const LETTER_W: u32 = 0x57;
const LETTER_A: u32 = 0x41;
const LETTER_Q: u32 = 0x51;
const LEFT_SHIFT: u32 = 0xA0;
const RIGHT_SHIFT: u32 = 0xA1;
const SHIFT: u32 = 0x10;
const CONTROL: u32 = 0x11;
const ALT: u32 = 0x12;
const MAC_FN: i64 = 63;
const MAC_LEFT_SHIFT: i64 = 56;
const MAC_RIGHT_SHIFT: i64 = 60;
const MAC_LEFT_COMMAND: i64 = 55;
const MAC_RIGHT_COMMAND: i64 = 54;
const MAC_LEFT_OPTION: i64 = 58;
const MAC_RIGHT_OPTION: i64 = 61;
const MAC_LEFT_CONTROL: i64 = 59;
const MAC_RIGHT_CONTROL: i64 = 62;
const MAC_ESCAPE: i64 = 53;
const MAC_SPACE: i64 = 49;
const MAC_S: i64 = 1;
const MAC_W: i64 = 13;
const MAC_A: i64 = 0;
const MAC_Q: i64 = 12;
const NONE_HELD: Physical = Physical {
    chord_keys: [false; 4],
    other_shortcut: false,
};

#[test]
fn mac_presets_keep_hands_free_shows_and_escape_without_claiming_left_modifiers() {
    use crate::MacShortcut;
    for (shortcut, flags, left_code, left_flag) in [
        (
            MacShortcut::Function,
            0x0080_0000,
            MAC_LEFT_OPTION,
            0x0008_0020,
        ),
        (
            MacShortcut::RightOption,
            0x0008_0040,
            MAC_LEFT_OPTION,
            0x0008_0020,
        ),
        (
            MacShortcut::RightCommand,
            0x0010_0010,
            MAC_LEFT_COMMAND,
            0x0010_0008,
        ),
    ] {
        let held = shortcut.modifiers(flags);
        assert!(held.talk && !held.other_shortcut);
        assert!(!shortcut.modifiers(left_flag).talk);
        assert!(shortcut.modifiers(left_flag).other_shortcut);
        for (code, expected) in [
            (MAC_SPACE, Action::Lock),
            (MAC_S, Action::Show(Scope::Screen)),
            (MAC_W, Action::Show(Scope::Window)),
            (MAC_A, Action::Show(Scope::Area)),
        ] {
            let mut policy = Mac::new(shortcut);
            assert!(
                policy
                    .observe(
                        MacEvent::FlagsChanged {
                            code: shortcut.key_code(),
                            held
                        },
                        || false,
                        now()
                    )
                    .starts()
            );
            let decision = policy.observe(
                key(code, true, held),
                || true,
                now() + Duration::from_millis(250),
            );
            assert_eq!(decision.action, Some(expected));
            assert!(decision.swallows());
            let escape = policy.observe(key(MAC_ESCAPE, true, held), || true, now());
            assert_eq!(escape.action, Some(Action::Escape));
            assert!(!escape.swallows());
            let other = policy.observe(
                MacEvent::FlagsChanged {
                    code: left_code,
                    held: shortcut.modifiers(flags | left_flag),
                },
                || true,
                now(),
            );
            assert_eq!(other.action, Some(Action::Interrupt));
        }
        let mut policy = Mac::new(shortcut);
        assert!(
            !policy
                .observe(
                    MacEvent::FlagsChanged {
                        code: shortcut.key_code(),
                        held: shortcut.modifiers(flags | left_flag)
                    },
                    || true,
                    now()
                )
                .starts()
        );
    }
}
#[test]
fn mac_alternative_modifiers_preserve_quick_application_shortcuts() {
    for shortcut in [
        crate::MacShortcut::RightOption,
        crate::MacShortcut::RightCommand,
    ] {
        for code in [MAC_S, MAC_W, MAC_A, MAC_SPACE] {
            let mut policy = Mac::new(shortcut);
            let held = FN_HELD;
            let start = now();
            policy.observe(
                MacEvent::FlagsChanged {
                    code: shortcut.key_code(),
                    held,
                },
                || true,
                start,
            );
            let decision = policy.observe(
                key(code, true, held),
                || true,
                start + Duration::from_millis(100),
            );
            assert!(!decision.swallows());
            assert_eq!(decision.action, Some(Action::Interrupt));
            assert!(
                !policy
                    .observe(
                        key(code, false, held),
                        || true,
                        start + Duration::from_millis(300)
                    )
                    .swallows()
            );
            assert_eq!(
                policy
                    .observe(
                        key(code, true, held),
                        || true,
                        start + Duration::from_millis(500)
                    )
                    .action,
                None
            );
        }
    }
}

/// What a macOS key event's flags report with no modifier held.
const NO_MODIFIERS: Modifiers = Modifiers {
    shift: false,
    other_shortcut: false,
    talk: false,
};
/// What a macOS key event's flags report while Fn is held.
const FN_HELD: Modifiers = Modifiers {
    talk: true,
    ..NO_MODIFIERS
};
/// What a macOS key event's flags report while Command is held.
const COMMAND_HELD: Modifiers = Modifiers {
    other_shortcut: true,
    ..NO_MODIFIERS
};

/// What Windows reports while `keys` are held.
fn held(keys: &[u32]) -> Physical {
    let control = keys.contains(&LEFT_CONTROL) || keys.contains(&RIGHT_CONTROL);
    Physical::read(|key| {
        let key = u32::from(key);
        keys.contains(&key) || (key == CONTROL && control)
    })
}

fn now() -> Instant {
    Instant::now()
}

fn function(down: bool) -> MacEvent {
    MacEvent::FlagsChanged {
        code: MAC_FN,
        held: Modifiers {
            talk: down,
            ..NO_MODIFIERS
        },
    }
}

/// Left Shift's flag change, down when `down`, beside the modifiers in `others`.
fn shift(down: bool, others: Modifiers) -> MacEvent {
    shift_on(MAC_LEFT_SHIFT, down, others)
}

/// The flag change of Shift key `code`, down when `down`, beside the modifiers in `others`.
fn shift_on(code: i64, down: bool, others: Modifiers) -> MacEvent {
    MacEvent::FlagsChanged {
        code,
        held: Modifiers {
            shift: down,
            ..others
        },
    }
}

fn key(code: i64, down: bool, held: Modifiers) -> MacEvent {
    MacEvent::Key { code, down, held }
}

/// The keyboard's own report while Fn is up.
fn keyboard_fn_up() -> bool {
    false
}

fn delivered(decision: &Decision) -> Vec<Action> {
    decision.actions().collect()
}

/// A Windows policy holding left Ctrl+Win, and what Windows reports while both are held.
fn windows_holding() -> (Windows, Physical) {
    let mut policy = Windows::default();
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    assert!(
        policy
            .observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), now())
            .starts()
    );
    (policy, held(&[LEFT_CONTROL, LEFT_WINDOWS]))
}

/// A macOS policy holding Fn.
fn mac_holding() -> Mac {
    let mut mac = Mac::default();
    assert!(mac.observe(function(true), keyboard_fn_up, now()).starts());
    mac
}

#[test]
fn shift_draws_beside_fn_but_any_modifier_holds_paste() {
    assert!(
        Modifiers {
            shift: true,
            ..FN_HELD
        }
        .draws(),
        "Shift must draw while Fn is held to talk"
    );
    assert!(
        !Modifiers {
            shift: true,
            ..COMMAND_HELD
        }
        .draws(),
        "A Shift shortcut must not draw"
    );
    assert!(FN_HELD.any(), "Fn alone must hold the paste");
    assert!(
        Modifiers {
            shift: true,
            ..NO_MODIFIERS
        }
        .any(),
        "Shift alone must hold the paste"
    );
    assert!(
        COMMAND_HELD.any(),
        "A shortcut modifier must hold the paste"
    );
    assert!(
        !NO_MODIFIERS.any(),
        "The paste waited with no modifier held"
    );
}

#[test]
fn shift_draws_beside_ctrl_win_but_not_in_another_windows_shortcut() {
    let modifiers = |keys: &[u32]| Windows::modifiers(|key| keys.contains(&u32::from(key)));
    assert!(
        modifiers(&[SHIFT, CONTROL, RIGHT_WINDOWS]).draws(),
        "Shift must draw while Ctrl+Win is still held after S"
    );
    assert!(
        !modifiers(&[SHIFT, LEFT_WINDOWS]).draws(),
        "Win+Shift, as in Win+Shift+S, must not draw"
    );
    assert!(
        !modifiers(&[SHIFT, CONTROL]).draws(),
        "Ctrl+Shift must not draw"
    );
    assert!(
        !modifiers(&[SHIFT, CONTROL, LEFT_WINDOWS, ALT]).draws(),
        "Alt beside the chord makes another shortcut"
    );
    assert!(
        modifiers(&[CONTROL, LEFT_WINDOWS]).any(),
        "The held chord must hold the paste"
    );
}

#[test]
fn interruption_revokes_authority_before_delivering_release() -> anyhow::Result<()> {
    let (sender, events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let permit = input.begin().context("Recording")?;
    Decision::pass(Some(Action::Interrupt)).deliver(&input);
    assert!(!permit.active());
    assert!(matches!(events.try_recv()?, Input::Interrupt));
    assert!(matches!(events.try_recv()?, Input::Release));
    Ok(())
}

#[test]
fn windows_sides_repeats_space_and_passive_escape() {
    let mut policy = Windows::default();
    assert_eq!(
        policy.observe(LEFT_CONTROL, true, NONE_HELD, now()).action,
        None
    );
    assert!(
        policy
            .observe(RIGHT_WINDOWS, true, held(&[LEFT_CONTROL]), now())
            .starts()
    );
    let both = held(&[LEFT_CONTROL, RIGHT_WINDOWS]);
    assert_eq!(
        policy.observe(RIGHT_WINDOWS, true, both, now()).action,
        None
    );
    assert!(policy.observe(SPACE, true, both, now()).swallows());
    assert_eq!(
        policy.observe(LEFT_CONTROL, false, both, now()).action,
        Some(Action::Release)
    );
    assert!(policy.observe(SPACE, false, NONE_HELD, now()).swallows());
    let escape = policy.observe(ESCAPE, true, NONE_HELD, now());
    assert!(!escape.swallows());
    assert_eq!(escape.action, Some(Action::Escape));
}

/// Escape during a hold is the keyboard's only way to cancel a Show, so it must reach the owner as
/// itself, never as another shortcut interrupting the hold.
#[test]
fn escape_during_a_hold_reaches_the_app_and_keeps_the_hold() {
    let (mut policy, both) = windows_holding();
    let escape = policy.observe(ESCAPE, true, both, now());
    assert!(!escape.swallows(), "Escape during a hold was swallowed");
    assert_eq!(delivered(&escape), [Action::Escape]);
    assert_eq!(delivered(&policy.observe(ESCAPE, false, both, now())), []);
    assert_eq!(
        delivered(&policy.observe(LEFT_CONTROL, false, both, now())),
        [Action::Release],
        "Escape interrupted the hold"
    );

    let mut mac = mac_holding();
    let escape = mac.observe(key(MAC_ESCAPE, true, FN_HELD), keyboard_fn_up, now());
    assert!(!escape.swallows(), "Escape during a hold was swallowed");
    assert_eq!(delivered(&escape), [Action::Escape]);
    assert_eq!(
        delivered(&mac.observe(key(MAC_ESCAPE, false, FN_HELD), keyboard_fn_up, now())),
        []
    );
    assert_eq!(
        delivered(&mac.observe(function(false), keyboard_fn_up, now())),
        [Action::Release],
        "Escape interrupted the hold"
    );
}

#[test]
fn s_during_a_hold_starts_a_show_and_is_swallowed_through_its_release() {
    let (mut policy, both) = windows_holding();
    let show = policy.observe(LETTER_S, true, both, now());
    assert!(show.swallows());
    assert_eq!(show.action, Some(Action::Show(Scope::Screen)));
    let drawing = policy.observe(LEFT_SHIFT, true, both, now());
    assert!(!drawing.swallows());
    assert_eq!(drawing.action, None, "Shift pressed to draw ended the Show");
    assert_eq!(
        policy.observe(LEFT_CONTROL, false, both, now()).action,
        Some(Action::Release)
    );
    assert!(
        policy.observe(LETTER_S, false, NONE_HELD, now()).swallows(),
        "S released after the chord reached the app"
    );
    let typed = policy.observe(LETTER_S, true, NONE_HELD, now());
    assert!(!typed.swallows(), "S typed with no hold was swallowed");
    assert_eq!(typed.action, None);

    let mut mac = mac_holding();
    let show = mac.observe(key(MAC_S, true, FN_HELD), keyboard_fn_up, now());
    assert!(show.swallows());
    assert_eq!(show.action, Some(Action::Show(Scope::Screen)));
    let drawing = mac.observe(shift(true, FN_HELD), keyboard_fn_up, now());
    assert!(!drawing.swallows());
    assert_eq!(
        drawing.action, None,
        "Shift pressed to draw ended the macOS Show"
    );
    assert_eq!(
        mac.observe(function(false), keyboard_fn_up, now()).action,
        Some(Action::Release)
    );
}

#[test]
fn w_and_a_during_a_hold_start_window_and_area_shows() {
    for (windows_key, mac_key, scope) in [
        (LETTER_W, MAC_W, Scope::Window),
        (LETTER_A, MAC_A, Scope::Area),
    ] {
        let (mut policy, both) = windows_holding();
        let show = policy.observe(windows_key, true, both, now());
        assert!(show.swallows());
        assert_eq!(show.action, Some(Action::Show(scope)));
        policy.observe(LEFT_CONTROL, false, both, now());
        assert!(
            policy
                .observe(windows_key, false, NONE_HELD, now())
                .swallows()
        );
        let typed = policy.observe(windows_key, true, NONE_HELD, now());
        assert!(!typed.swallows());
        assert_eq!(typed.action, None);

        let mut mac = mac_holding();
        let show = mac.observe(key(mac_key, true, FN_HELD), keyboard_fn_up, now());
        assert!(show.swallows());
        assert_eq!(show.action, Some(Action::Show(scope)));
        assert!(
            mac.observe(key(mac_key, false, FN_HELD), keyboard_fn_up, now())
                .swallows()
        );
    }
}

/// Replays key events, each `(milliseconds, key, down)`, through the Windows policy with nothing
/// else held, and returns the double taps that asked for a pin, from the first press to the second
/// release in milliseconds.
fn windows_pins(events: &[(u64, u32, bool)]) -> Vec<(u64, u64)> {
    let start = now();
    let millis = |at: Instant| u64::try_from(at.duration_since(start).as_millis()).unwrap();
    let mut policy = Windows::default();
    events
        .iter()
        .filter_map(|&(offset, key, down)| {
            let at = start.checked_add(Duration::from_millis(offset)).unwrap();
            let decision = policy.observe(key, down, NONE_HELD, at);
            assert!(!decision.swallows(), "Shift and typing stay with the app");
            match decision.action? {
                Action::Pin(taps) => Some((millis(taps.pressed), millis(taps.released))),
                action => panic!("Shift taps asked for {action:?}"),
            }
        })
        .collect()
}

#[test]
fn two_quick_taps_of_shift_alone_drop_a_pin() {
    let tap = |at: u64| [(at, LEFT_SHIFT, true), (at + 80, LEFT_SHIFT, false)];
    assert_eq!(windows_pins(&[tap(0), tap(300)].concat()), [(0, 380)]);
    let repeated = [
        (0, LEFT_SHIFT, true),
        (40, LEFT_SHIFT, true),
        (90, LEFT_SHIFT, false),
    ];
    assert_eq!(
        windows_pins(&[&repeated[..], &tap(250)].concat()),
        [(0, 330)],
        "Autorepeat while the first tap is held"
    );
    assert_eq!(windows_pins(&[tap(0), tap(700)].concat()), [], "Too slow");
    let long_press = [(0, LEFT_SHIFT, true), (600, LEFT_SHIFT, false)];
    assert_eq!(
        windows_pins(&[&long_press[..], &tap(700)].concat()),
        [],
        "Shift held to draw is no tap"
    );
    let typed = [(150, LETTER_Q, true), (160, LETTER_Q, false)];
    assert_eq!(
        windows_pins(&[&tap(0)[..], &typed, &tap(200)].concat()),
        [],
        "A key between"
    );
    assert_eq!(
        windows_pins(&[tap(0), tap(200), tap(400)].concat()),
        [(0, 280), (200, 480)],
        "Each tap also begins the next pair, for a quick stroke the Show skips"
    );
}

#[test]
fn shift_taps_pin_beside_the_held_talk_shortcut() {
    let start = now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let taps = Taps {
        pressed: at(500),
        released: at(780),
    };
    let shift_keys = [(LEFT_SHIFT, MAC_LEFT_SHIFT), (RIGHT_SHIFT, MAC_RIGHT_SHIFT)];
    for (windows_shift, mac_shift) in shift_keys {
        let (mut policy, both) = windows_holding();
        policy.observe(LETTER_S, true, both, at(50));
        policy.observe(LETTER_S, false, both, at(100));
        let tap = |policy: &mut Windows, millis: u64| {
            policy.observe(windows_shift, true, both, at(millis));
            policy
                .observe(windows_shift, false, both, at(millis + 80))
                .action
        };
        assert_eq!(tap(&mut policy, 500), None);
        // The held shortcut's autorepeat comes between the taps.
        policy.observe(LEFT_CONTROL, true, both, at(640));
        assert_eq!(
            tap(&mut policy, 700),
            Some(Action::Pin(taps)),
            "Key {windows_shift:#X}"
        );

        let mut mac = mac_holding();
        let pins: Vec<Option<Action>> = [(500, true), (580, false), (700, true), (780, false)]
            .into_iter()
            .map(|(millis, down)| {
                let event = shift_on(mac_shift, down, FN_HELD);
                mac.observe(event, keyboard_fn_up, at(millis)).action
            })
            .collect();
        assert_eq!(
            pins,
            [None, None, None, Some(Action::Pin(taps))],
            "Key code {mac_shift}"
        );
    }
}

#[test]
fn shift_taps_inside_another_shortcut_never_pin() {
    let start = now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let mut policy = Windows::default();
    let pins: Vec<Option<Action>> = [0, 300]
        .into_iter()
        .map(|millis| {
            policy.observe(LEFT_CONTROL, true, NONE_HELD, at(millis));
            policy.observe(LEFT_SHIFT, true, held(&[LEFT_CONTROL]), at(millis + 20));
            let released =
                policy.observe(LEFT_SHIFT, false, held(&[LEFT_CONTROL]), at(millis + 80));
            policy.observe(LEFT_CONTROL, false, held(&[LEFT_CONTROL]), at(millis + 100));
            released.action
        })
        .collect();
    assert_eq!(pins, [None, None], "Ctrl+Shift twice is a shortcut");
    for physical in [held(&[ALT]), held(&[LEFT_CONTROL])] {
        let mut policy = Windows::default();
        let released: Vec<Option<Action>> = [0, 200]
            .into_iter()
            .map(|millis| {
                policy.observe(LEFT_SHIFT, true, physical, at(millis));
                policy
                    .observe(LEFT_SHIFT, false, physical, at(millis + 80))
                    .action
            })
            .collect();
        assert_eq!(
            released,
            [None, None],
            "Shift taps beside Alt or Ctrl alone, as in a layout switch, pinned"
        );
    }

    let option = |down| MacEvent::FlagsChanged {
        code: MAC_LEFT_OPTION,
        held: Modifiers {
            other_shortcut: down,
            ..FN_HELD
        },
    };
    let mut mac = mac_holding();
    mac.observe(shift(true, FN_HELD), keyboard_fn_up, at(0));
    mac.observe(shift(false, FN_HELD), keyboard_fn_up, at(80));
    mac.observe(option(true), keyboard_fn_up, at(100));
    mac.observe(option(false), keyboard_fn_up, at(120));
    mac.observe(shift(true, FN_HELD), keyboard_fn_up, at(200));
    assert_eq!(
        mac.observe(shift(false, FN_HELD), keyboard_fn_up, at(280))
            .action,
        None,
        "Option between the taps let them pin"
    );
    let mut mac = Mac::default();
    let released: Vec<Option<Action>> = [(0, true), (80, false), (200, true), (280, false)]
        .into_iter()
        .map(|(millis, down)| {
            mac.observe(shift(down, COMMAND_HELD), keyboard_fn_up, at(millis))
                .action
        })
        .collect();
    assert_eq!(released, [None; 4], "Shift taps with Command held pinned");
}

#[test]
fn windows_lifecycle_interruption_waits_for_chord_release() {
    let (sender, _events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let (mut policy, both) = windows_holding();
    policy.interrupt(&input);
    assert_eq!(
        policy.observe(RIGHT_WINDOWS, true, both, now()).action,
        None
    );
    assert_eq!(
        policy
            .observe(RIGHT_WINDOWS, false, NONE_HELD, now())
            .action,
        None
    );
    policy.observe(RIGHT_CONTROL, true, NONE_HELD, now());
    assert!(
        policy
            .observe(RIGHT_WINDOWS, true, held(&[RIGHT_CONTROL]), now())
            .starts()
    );
}

#[test]
fn windows_missed_release_and_other_shortcuts_do_not_restart_hold() {
    let (mut policy, both) = windows_holding();
    assert_eq!(
        policy.observe(LETTER_Q, true, both, now()).action,
        Some(Action::Interrupt)
    );
    assert!(!policy.observe(RIGHT_WINDOWS, true, both, now()).starts());
    assert_eq!(
        policy
            .observe(RIGHT_WINDOWS, false, NONE_HELD, now())
            .action,
        None
    );
    assert_eq!(
        policy.observe(RIGHT_CONTROL, true, NONE_HELD, now()).action,
        None
    );
    assert!(
        policy
            .observe(RIGHT_WINDOWS, true, held(&[RIGHT_CONTROL]), now())
            .starts()
    );
    assert_eq!(
        delivered(&policy.observe(LEFT_CONTROL, true, NONE_HELD, now())),
        [Action::Release]
    );
}

#[test]
fn a_missed_release_lets_s_reach_the_app() {
    // Ctrl+Win went up on the secure desktop, where the hook sees nothing.
    let (mut policy, _) = windows_holding();
    let typed = policy.observe(LETTER_S, true, NONE_HELD, now());
    assert!(!typed.swallows(), "S after a missed release was swallowed");
    assert_eq!(delivered(&typed), [Action::Release]);
    assert!(!policy.observe(LETTER_S, false, NONE_HELD, now()).swallows());

    // The tap missed Fn's release, so the next key's flags show Fn up.
    let mut mac = mac_holding();
    let typed = mac.observe(key(MAC_S, true, NO_MODIFIERS), keyboard_fn_up, now());
    assert!(
        !typed.swallows(),
        "S after a missed Fn release was swallowed"
    );
    assert_eq!(delivered(&typed), [Action::Release]);
    assert!(mac.observe(function(true), keyboard_fn_up, now()).starts());
    assert_eq!(
        delivered(&mac.observe(function(true), keyboard_fn_up, now())),
        [Action::Release, Action::Press],
        "Fn has one key, so a second press must end the hold whose release went unseen"
    );
}

#[test]
fn a_show_key_whose_release_went_unseen_asks_for_no_show_outside_a_hold() {
    // S and Ctrl+Win went up on the secure desktop, so S typed afterward belongs to no hold.
    let (mut policy, both) = windows_holding();
    policy.observe(LETTER_S, true, both, now());
    let typed = policy.observe(LETTER_S, true, NONE_HELD, now());
    assert_eq!(delivered(&typed), [Action::Release]);

    // The tap went off while S was held, and S and Fn came up unseen.
    let (sender, _events) = async_channel::bounded(4);
    let mut mac = mac_holding();
    mac.observe(key(MAC_S, true, FN_HELD), keyboard_fn_up, now());
    mac.missed_events(&InputSender::new(sender));
    let typed = mac.observe(key(MAC_S, true, NO_MODIFIERS), keyboard_fn_up, now());
    assert_eq!(
        delivered(&typed),
        [],
        "S typed after the outage asked for a Show"
    );
}

#[test]
fn a_posted_event_without_fn_ends_no_hold_the_keyboard_still_makes() {
    let keyboard_fn_down = || true;
    let mut mac = mac_holding();
    // Another app posts a key release and a Command release whose flags leave Fn out.
    let posted = mac.observe(key(MAC_Q, false, NO_MODIFIERS), keyboard_fn_down, now());
    assert_eq!(delivered(&posted), [], "A posted key-up ended the hold");
    let command_up = MacEvent::FlagsChanged {
        code: MAC_LEFT_COMMAND,
        held: NO_MODIFIERS,
    };
    assert_eq!(
        delivered(&mac.observe(command_up, keyboard_fn_down, now())),
        []
    );
    let show = mac.observe(key(MAC_S, true, FN_HELD), keyboard_fn_down, now());
    assert_eq!(show.action, Some(Action::Show(Scope::Screen)));
}

#[test]
fn a_tap_outage_ends_a_hold_but_not_a_released_dictation() -> anyhow::Result<()> {
    let (sender, events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let mut mac = mac_holding();
    assert_eq!(
        delivered(&mac.observe(function(false), keyboard_fn_up, now())),
        [Action::Release]
    );
    let permit = input.begin().context("Recording")?;
    mac.missed_events(&input);
    assert!(
        permit.active(),
        "A tap outage after Fn's release cancelled the paste"
    );
    assert!(events.try_recv().is_err());

    mac.observe(shift(true, NO_MODIFIERS), keyboard_fn_up, now());
    mac.observe(shift(false, NO_MODIFIERS), keyboard_fn_up, now());
    mac.missed_events(&input);
    mac.observe(shift(true, NO_MODIFIERS), keyboard_fn_up, now());
    assert_eq!(
        delivered(&mac.observe(shift(false, NO_MODIFIERS), keyboard_fn_up, now())),
        [],
        "Shift taps across the outage pinned"
    );

    assert!(mac.observe(function(true), keyboard_fn_up, now()).starts());
    mac.missed_events(&input);
    assert!(matches!(events.try_recv()?, Input::Interrupt));
    assert!(matches!(events.try_recv()?, Input::Release));
    assert_eq!(
        delivered(&mac.observe(function(false), keyboard_fn_up, now())),
        []
    );
    Ok(())
}

#[test]
fn command_option_or_control_during_fn_is_another_shortcut() {
    let shortcut = Modifiers {
        other_shortcut: true,
        ..FN_HELD
    };
    for code in [
        MAC_LEFT_COMMAND,
        MAC_RIGHT_COMMAND,
        MAC_LEFT_OPTION,
        MAC_RIGHT_OPTION,
        MAC_LEFT_CONTROL,
        MAC_RIGHT_CONTROL,
    ] {
        let mut mac = mac_holding();
        let pressed = mac.observe(
            MacEvent::FlagsChanged {
                code,
                held: shortcut,
            },
            keyboard_fn_up,
            now(),
        );
        assert!(!pressed.swallows());
        assert_eq!(pressed.action, Some(Action::Interrupt), "Key code {code}");
        let typed = mac.observe(key(MAC_S, true, shortcut), keyboard_fn_up, now());
        assert!(!typed.swallows(), "Key code {code} let S start a Show");
        assert_eq!(delivered(&typed), []);
    }
    let mut mac = mac_holding();
    let released = MacEvent::FlagsChanged {
        code: MAC_LEFT_COMMAND,
        held: FN_HELD,
    };
    assert_eq!(
        delivered(&mac.observe(released, keyboard_fn_up, now())),
        [],
        "Releasing Command held since before Fn ended the hold"
    );
    let show = mac.observe(key(MAC_S, true, FN_HELD), keyboard_fn_up, now());
    assert_eq!(show.action, Some(Action::Show(Scope::Screen)));
}

#[test]
fn mac_lifecycle_interruption_waits_for_fn_release() -> anyhow::Result<()> {
    let (sender, events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let mut policy = mac_holding();
    policy.interrupt(&input);
    assert!(matches!(events.try_recv()?, Input::Interrupt));
    assert!(matches!(events.try_recv()?, Input::Release));
    let typed = policy.observe(key(MAC_S, true, FN_HELD), keyboard_fn_up, now());
    assert!(
        !typed.swallows(),
        "S during an interrupted hold was swallowed"
    );
    assert_eq!(delivered(&typed), []);
    assert_eq!(
        delivered(&policy.observe(function(false), keyboard_fn_up, now())),
        []
    );
    assert!(
        policy
            .observe(function(true), keyboard_fn_up, now())
            .starts()
    );

    // Fn went up while the Mac slept, unseen; the first press after waking still starts.
    policy.interrupt(&input);
    assert!(
        policy
            .observe(function(true), keyboard_fn_up, now())
            .starts(),
        "The first press after an unseen release was lost"
    );
    Ok(())
}

#[test]
fn mac_function_space_and_escape_are_observed_without_native_events() {
    let mut policy = mac_holding();
    assert!(
        policy
            .observe(key(MAC_SPACE, true, FN_HELD), keyboard_fn_up, now())
            .swallows()
    );
    assert_eq!(
        policy
            .observe(function(false), keyboard_fn_up, now())
            .action,
        Some(Action::Release)
    );
    assert!(
        policy
            .observe(key(MAC_SPACE, false, NO_MODIFIERS), keyboard_fn_up, now())
            .swallows()
    );
    let escape = policy.observe(key(MAC_ESCAPE, true, NO_MODIFIERS), keyboard_fn_up, now());
    assert_eq!(escape.action, Some(Action::Escape));
    assert!(!escape.swallows());
    assert!(
        policy
            .observe(function(true), keyboard_fn_up, now())
            .starts()
    );
    assert_eq!(
        policy
            .observe(key(MAC_Q, true, FN_HELD), keyboard_fn_up, now())
            .action,
        Some(Action::Interrupt)
    );
    assert!(
        !policy
            .observe(key(MAC_SPACE, true, FN_HELD), keyboard_fn_up, now())
            .swallows(),
        "Space after another shortcut interrupted the hold was swallowed"
    );
    assert_eq!(
        delivered(&policy.observe(function(false), keyboard_fn_up, now())),
        []
    );
    assert!(
        policy
            .observe(function(true), keyboard_fn_up, now())
            .starts()
    );
}

#[test]
fn hands_free_space_stays_swallowed_until_its_release() {
    let mut chord = Chord::default();
    assert_eq!(chord.press(), Some(Action::Press));
    assert!(chord.hold_key(HoldKey::Space, true).swallows());
    assert!(
        chord.hold_key(HoldKey::Space, true).swallows(),
        "A held shortcut must swallow Space repeats"
    );
    assert_eq!(chord.release(), Some(Action::Release));
    assert!(
        chord.hold_key(HoldKey::Space, true).swallows(),
        "Releasing the shortcut must not leak a held Space into the editor"
    );
    assert!(chord.hold_key(HoldKey::Space, false).swallows());
    assert!(
        !chord.hold_key(HoldKey::Space, true).swallows(),
        "A fresh Space belongs to the editor"
    );
    assert!(!chord.hold_key(HoldKey::Space, false).swallows());
}
