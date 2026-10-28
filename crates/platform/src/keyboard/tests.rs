//! Drives the Ctrl+Win and Fn policies with synthetic key events and the held-modifier rules with
//! chosen key states; no test installs a hook or reads the keyboard.

use std::time::{Duration, Instant};

use anyhow::Context;

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
const ALT: u32 = 0x12;
const MAC_LEFT_SHIFT: i64 = 56;
const MAC_Q: i64 = 12;
const NONE_HELD: Physical = Physical {
    chord_keys: [false; 4],
    shortcut: false,
};
/// What a macOS key event's flags report while Fn is held.
const FN_HELD: Modifiers = Modifiers {
    shift: false,
    shortcut: false,
    talk: true,
};
/// What a macOS key event's flags report with no modifier held.
const NO_MODIFIERS: Modifiers = Modifiers {
    shift: false,
    shortcut: false,
    talk: false,
};

/// What Windows reports while `keys` are held.
fn held(keys: &[u32]) -> Physical {
    let down = |key: u16| keys.contains(&u32::from(key));
    let control = keys.contains(&LEFT_CONTROL) || keys.contains(&RIGHT_CONTROL);
    Physical {
        chord_keys: Windows::CHORD_KEYS.map(down),
        shortcut: Windows::modifiers(|key| down(key) || key == Windows::CONTROL && control)
            .shortcut,
    }
}

fn now() -> Instant {
    Instant::now()
}

fn function(down: bool) -> MacEvent {
    MacEvent::FlagsChanged {
        code: Mac::FUNCTION,
        held: Modifiers {
            talk: down,
            ..Modifiers::default()
        },
    }
}

fn key(code: i64, down: bool, held: Modifiers) -> MacEvent {
    MacEvent::Key { code, down, held }
}

fn delivered(decision: &Decision) -> Vec<Action> {
    decision.actions().collect()
}

#[test]
fn shift_draws_beside_fn_but_any_modifier_holds_paste() {
    let held = |shift, shortcut, talk| Modifiers {
        shift,
        shortcut,
        talk,
    };
    assert!(
        held(true, false, true).draws(),
        "Shift must draw while Fn is held to talk"
    );
    assert!(
        !held(true, true, false).draws(),
        "A Shift shortcut must not draw"
    );
    assert!(
        held(false, false, true).any(),
        "Fn alone must hold the paste"
    );
    assert!(
        held(true, false, false).any(),
        "Shift alone must hold the paste"
    );
    assert!(
        held(false, true, false).any(),
        "A shortcut modifier must hold the paste"
    );
    assert!(!Modifiers::default().any());
}

#[test]
fn shift_draws_beside_ctrl_win_but_not_in_another_windows_shortcut() {
    let held = |keys: &[u16]| Windows::modifiers(|key| keys.contains(&key));
    let (shift, control) = (Windows::SHIFT, Windows::CONTROL);
    assert!(
        held(&[shift, control, Windows::RIGHT_WINDOWS]).draws(),
        "Shift must draw while Ctrl+Win is still held after S"
    );
    assert!(
        !held(&[shift, Windows::LEFT_WINDOWS]).draws(),
        "Win+Shift, as in Win+Shift+S, must not draw"
    );
    assert!(!held(&[shift, control]).draws(), "Ctrl+Shift must not draw");
    assert!(
        !held(&[shift, control, Windows::LEFT_WINDOWS, Windows::ALT]).draws(),
        "Alt beside the chord makes another shortcut"
    );
    assert!(
        held(&[control, Windows::LEFT_WINDOWS]).any(),
        "The held chord must hold the paste"
    );
}

#[test]
fn interruption_revokes_authority_before_delivering_release() -> anyhow::Result<()> {
    let (sender, events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let permit = input.begin().context("Missing permit")?;
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
    assert_eq!(escape.action, Some(Action::Cancel));
}

#[test]
fn s_during_a_hold_starts_a_show_and_is_swallowed_through_its_release() {
    let mut policy = Windows::default();
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    policy.observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), now());
    let both = held(&[LEFT_CONTROL, LEFT_WINDOWS]);
    let show = policy.observe(LETTER_S, true, both, now());
    assert!(show.swallows());
    assert_eq!(show.action, Some(Action::Show(Scope::Screen)));
    // Reaching for Shift to draw before letting go of the chord keeps the Show.
    let shift = policy.observe(LEFT_SHIFT, true, both, now());
    assert!(!shift.swallows());
    assert_eq!(shift.action, None);
    // The chord's release follows, but S's own release still belongs to the shortcut.
    assert_eq!(
        policy.observe(LEFT_CONTROL, false, both, now()).action,
        Some(Action::Release)
    );
    assert!(policy.observe(LETTER_S, false, NONE_HELD, now()).swallows());
    // Typing S with no hold reaches the app untouched.
    let typed = policy.observe(LETTER_S, true, NONE_HELD, now());
    assert!(!typed.swallows());
    assert_eq!(typed.action, None);

    let mut mac = Mac::default();
    mac.observe(function(true), now());
    let show = mac.observe(key(Mac::SCREEN, true, FN_HELD), now());
    assert!(show.swallows());
    assert_eq!(show.action, Some(Action::Show(Scope::Screen)));
    // macOS reports Shift as a flag change while Fn stays held, and it keeps the Show too.
    let shift = mac.observe(
        MacEvent::FlagsChanged {
            code: MAC_LEFT_SHIFT,
            held: Modifiers {
                shift: true,
                talk: true,
                ..Modifiers::default()
            },
        },
        now(),
    );
    assert!(!shift.swallows());
    assert_eq!(shift.action, None);
    assert_eq!(
        mac.observe(function(false), now()).action,
        Some(Action::Release)
    );
}

#[test]
fn w_and_a_during_a_hold_start_window_and_area_shows() {
    for (windows_key, mac_key, scope) in [
        (LETTER_W, Mac::WINDOW, Scope::Window),
        (LETTER_A, Mac::AREA, Scope::Area),
    ] {
        let mut policy = Windows::default();
        policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
        policy.observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), now());
        let both = held(&[LEFT_CONTROL, LEFT_WINDOWS]);
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

        let mut mac = Mac::default();
        mac.observe(function(true), now());
        let show = mac.observe(key(mac_key, true, FN_HELD), now());
        assert!(show.swallows());
        assert_eq!(show.action, Some(Action::Show(scope)));
        assert!(mac.observe(key(mac_key, false, FN_HELD), now()).swallows());
    }
}

/// Replays Shift and other key events, each `(milliseconds, shift, down)`, through the Windows
/// policy and returns the double taps that asked for a pin, from the first press to the second
/// release in milliseconds; `shift` false is the letter Q.
fn windows_pins(events: &[(u64, bool, bool)]) -> Vec<(u64, u64)> {
    let start = now();
    let millis = |at: Instant| u64::try_from(at.duration_since(start).as_millis()).unwrap();
    let mut policy = Windows::default();
    events
        .iter()
        .filter_map(|&(offset, shift, down)| {
            let key = if shift { LEFT_SHIFT } else { LETTER_Q };
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
    let tap = |at: u64| [(at, true, true), (at + 80, true, false)];
    assert_eq!(windows_pins(&[tap(0), tap(300)].concat()), [(0, 380)]);
    // Autorepeat while the first tap is held changes nothing.
    let repeated = [(0, true, true), (40, true, true), (90, true, false)];
    assert_eq!(
        windows_pins(&[&repeated[..], &tap(250)].concat()),
        [(0, 330)]
    );
    assert_eq!(windows_pins(&[tap(0), tap(700)].concat()), [], "too slow");
    let held = [(0, true, true), (600, true, false)];
    assert_eq!(
        windows_pins(&[&held[..], &tap(700)].concat()),
        [],
        "Shift held to draw is no tap"
    );
    let typed = [(150, false, true), (160, false, false)];
    assert_eq!(
        windows_pins(&[&tap(0)[..], &typed, &tap(200)].concat()),
        [],
        "a key between"
    );
    assert_eq!(
        windows_pins(&[tap(0), tap(200), tap(400)].concat()),
        [(0, 280), (200, 480)],
        "each tap also begins the next pair, for a quick stroke the Show skips"
    );
}

#[test]
fn shift_taps_pin_beside_the_held_talk_shortcut() {
    let start = now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let mut policy = Windows::default();
    policy.observe(LEFT_CONTROL, true, NONE_HELD, at(0));
    policy.observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), at(0));
    let both = held(&[LEFT_CONTROL, LEFT_WINDOWS]);
    policy.observe(LETTER_S, true, both, at(50));
    policy.observe(LETTER_S, false, both, at(100));
    let tap = |policy: &mut Windows, millis: u64| {
        policy.observe(LEFT_SHIFT, true, both, at(millis));
        policy
            .observe(LEFT_SHIFT, false, both, at(millis + 80))
            .action
    };
    assert_eq!(tap(&mut policy, 500), None);
    // The held shortcut's autorepeat comes between the taps.
    policy.observe(LEFT_CONTROL, true, both, at(640));
    let taps = Taps {
        pressed: at(500),
        released: at(780),
    };
    assert_eq!(tap(&mut policy, 700), Some(Action::Pin(taps)));

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
    // Alt, or Ctrl alone, held through both taps makes them part of another shortcut, such as a
    // keyboard layout switch.
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
        assert_eq!(released, [None, None]);
    }

    let shift = |down: bool, code: i64| MacEvent::FlagsChanged {
        code,
        held: Modifiers {
            shift: down,
            talk: true,
            ..Modifiers::default()
        },
    };
    let mut mac = Mac::default();
    mac.observe(function(true), at(0));
    let pins: Vec<Option<Action>> = [(500, true), (580, false), (700, true), (780, false)]
        .into_iter()
        .map(|(millis, down)| mac.observe(shift(down, MAC_LEFT_SHIFT), at(millis)).action)
        .collect();
    assert_eq!(pins, [None, None, None, Some(Action::Pin(taps))]);
    // Another modifier between the taps, here left Option, makes a different shortcut.
    mac.observe(shift(true, MAC_LEFT_SHIFT), at(1_000));
    mac.observe(shift(false, MAC_LEFT_SHIFT), at(1_080));
    mac.observe(shift(true, 58), at(1_100));
    mac.observe(shift(true, MAC_LEFT_SHIFT), at(1_200));
    assert_eq!(
        mac.observe(shift(false, MAC_LEFT_SHIFT), at(1_280)).action,
        None
    );
    // So does Command held through both taps.
    let command = |down: bool| MacEvent::FlagsChanged {
        code: MAC_LEFT_SHIFT,
        held: Modifiers {
            shift: down,
            shortcut: true,
            ..Modifiers::default()
        },
    };
    let mut mac = Mac::default();
    let released: Vec<Option<Action>> = [(0, true), (80, false), (200, true), (280, false)]
        .into_iter()
        .map(|(millis, down)| mac.observe(command(down), at(millis)).action)
        .collect();
    assert_eq!(released, [None; 4]);
}

#[test]
fn windows_lifecycle_interruption_waits_for_chord_release() {
    let (sender, _events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let mut policy = Windows::default();
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    assert!(
        policy
            .observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), now())
            .starts()
    );
    policy.interrupt(&input);
    assert_eq!(
        policy
            .observe(
                RIGHT_WINDOWS,
                true,
                held(&[LEFT_CONTROL, LEFT_WINDOWS]),
                now()
            )
            .action,
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
    let mut policy = Windows::default();
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    assert!(
        policy
            .observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), now())
            .starts()
    );
    let both = held(&[LEFT_CONTROL, LEFT_WINDOWS]);
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
    let mut policy = Windows::default();
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    assert!(
        policy
            .observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), now())
            .starts()
    );
    let typed = policy.observe(LETTER_S, true, NONE_HELD, now());
    assert!(!typed.swallows(), "S after a missed release was swallowed");
    assert_eq!(delivered(&typed), [Action::Release]);
    assert!(!policy.observe(LETTER_S, false, NONE_HELD, now()).swallows());

    // The tap missed Fn's release, so the next key's flags show Fn up.
    let mut mac = Mac::default();
    assert!(mac.observe(function(true), now()).starts());
    let typed = mac.observe(key(Mac::SCREEN, true, NO_MODIFIERS), now());
    assert!(
        !typed.swallows(),
        "S after a missed Fn release was swallowed"
    );
    assert_eq!(delivered(&typed), [Action::Release]);
    assert!(mac.observe(function(true), now()).starts());
    // Fn has one key, so a second press proves the release between them was missed.
    assert_eq!(
        delivered(&mac.observe(function(true), now())),
        [Action::Release, Action::Press]
    );
}

#[test]
fn mac_lifecycle_interruption_waits_for_fn_release() -> anyhow::Result<()> {
    let (sender, events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let mut policy = Mac::default();
    assert!(policy.observe(function(true), now()).starts());
    policy.interrupt(&input);
    assert!(matches!(events.try_recv()?, Input::Interrupt));
    assert!(matches!(events.try_recv()?, Input::Release));
    let typed = policy.observe(key(Mac::SCREEN, true, FN_HELD), now());
    assert!(
        !typed.swallows(),
        "S during an interrupted hold was swallowed"
    );
    assert_eq!(delivered(&typed), []);
    assert_eq!(delivered(&policy.observe(function(false), now())), []);
    assert!(policy.observe(function(true), now()).starts());

    // Fn went up while the Mac slept, unseen; the first press after waking still starts.
    policy.interrupt(&input);
    assert!(
        policy.observe(function(true), now()).starts(),
        "The first press after an unseen release was lost"
    );
    Ok(())
}

#[test]
fn mac_function_space_and_escape_are_observed_without_native_events() {
    let mut policy = Mac::default();
    assert!(policy.observe(function(true), now()).starts());
    assert!(
        policy
            .observe(key(Mac::SPACE, true, FN_HELD), now())
            .swallows()
    );
    assert_eq!(
        policy.observe(function(false), now()).action,
        Some(Action::Release)
    );
    assert!(
        policy
            .observe(key(Mac::SPACE, false, NO_MODIFIERS), now())
            .swallows()
    );
    let escape = policy.observe(key(Mac::ESCAPE, true, NO_MODIFIERS), now());
    assert_eq!(escape.action, Some(Action::Cancel));
    assert!(!escape.swallows());
    assert!(policy.observe(function(true), now()).starts());
    assert_eq!(
        policy.observe(key(MAC_Q, true, FN_HELD), now()).action,
        Some(Action::Interrupt)
    );
    assert!(
        !policy
            .observe(key(Mac::SPACE, true, FN_HELD), now())
            .swallows(),
        "Space after another shortcut interrupted the hold was swallowed"
    );
    assert_eq!(delivered(&policy.observe(function(false), now())), []);
    assert!(policy.observe(function(true), now()).starts());
}

#[test]
fn hands_free_space_press_and_release_never_reach_the_app() {
    let mut chord = Chord::default();
    assert_eq!(chord.press(), Some(Action::Press));
    assert!(chord.hold_key(HoldKey::Space, true).swallows());
    assert!(chord.hold_key(HoldKey::Space, false).swallows());
    assert!(!chord.hold_key(HoldKey::Space, false).swallows());
    assert_eq!(chord.release(), Some(Action::Release));
    assert!(
        !chord.hold_key(HoldKey::Space, true).swallows(),
        "Space was swallowed without a hold"
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
