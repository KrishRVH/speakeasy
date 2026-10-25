//! Drives the Ctrl+Win and Fn policies with synthetic key events and the held-modifier rules with
//! chosen key states; no test installs a hook or reads the keyboard.

use anyhow::Context;

use super::*;

const LEFT_CONTROL: u32 = 0xA2;
const RIGHT_CONTROL: u32 = 0xA3;
const LEFT_WINDOWS: u32 = 0x5B;
const RIGHT_WINDOWS: u32 = 0x5C;
const ESCAPE: u32 = 0x1B;
const SPACE: u32 = 0x20;
const LETTER_S: u32 = 0x53;
const LETTER_A: u32 = 0x41;
const LEFT_SHIFT: u32 = 0xA0;
const MAC_LEFT_SHIFT: i64 = 56;
const NONE_HELD: [bool; 4] = [false; 4];

fn held(keys: &[u32]) -> [bool; 4] {
    Windows::CHORD_KEYS.map(|chord_key| keys.contains(&u32::from(chord_key)))
}

fn function(down: bool) -> MacEvent {
    MacEvent::FlagsChanged {
        code: Mac::FUNCTION,
        function: down,
    }
}

fn key(code: i64, down: bool) -> MacEvent {
    MacEvent::Key { code, down }
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
    assert!(matches!(events.try_recv()?, Input::Cancel));
    assert!(matches!(events.try_recv()?, Input::Release));
    Ok(())
}

#[test]
fn windows_sides_repeats_space_and_passive_escape() {
    let mut policy = Windows::default();
    assert_eq!(policy.observe(LEFT_CONTROL, true, NONE_HELD).action, None);
    assert!(
        policy
            .observe(RIGHT_WINDOWS, true, held(&[LEFT_CONTROL]))
            .starts()
    );
    let both = held(&[LEFT_CONTROL, RIGHT_WINDOWS]);
    assert_eq!(policy.observe(RIGHT_WINDOWS, true, both).action, None);
    assert!(policy.observe(SPACE, true, both).swallows());
    assert_eq!(
        policy.observe(LEFT_CONTROL, false, both).action,
        Some(Action::Release)
    );
    assert!(policy.observe(SPACE, false, NONE_HELD).swallows());
    let escape = policy.observe(ESCAPE, true, NONE_HELD);
    assert!(!escape.swallows());
    assert_eq!(escape.action, Some(Action::Cancel));
}

#[test]
fn s_during_a_hold_starts_a_show_and_is_swallowed_through_its_release() {
    let mut policy = Windows::default();
    policy.observe(LEFT_CONTROL, true, NONE_HELD);
    policy.observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]));
    let both = held(&[LEFT_CONTROL, LEFT_WINDOWS]);
    let show = policy.observe(LETTER_S, true, both);
    assert!(show.swallows());
    assert_eq!(show.action, Some(Action::Show));
    // Reaching for Shift to draw before letting go of the chord keeps the Show.
    let shift = policy.observe(LEFT_SHIFT, true, both);
    assert!(!shift.swallows());
    assert_eq!(shift.action, None);
    // The chord's release follows, but S's own release still belongs to the shortcut.
    assert_eq!(
        policy.observe(LEFT_CONTROL, false, both).action,
        Some(Action::Release)
    );
    assert!(policy.observe(LETTER_S, false, NONE_HELD).swallows());
    // Typing S with no hold reaches the app untouched.
    let typed = policy.observe(LETTER_S, true, NONE_HELD);
    assert!(!typed.swallows());
    assert_eq!(typed.action, None);

    let mut mac = Mac::default();
    mac.observe(function(true));
    let show = mac.observe(key(Mac::SHOW, true));
    assert!(show.swallows());
    assert_eq!(show.action, Some(Action::Show));
    // macOS reports Shift as a flag change while Fn stays held, and it keeps the Show too.
    let shift = mac.observe(MacEvent::FlagsChanged {
        code: MAC_LEFT_SHIFT,
        function: true,
    });
    assert!(!shift.swallows());
    assert_eq!(shift.action, None);
    assert_eq!(mac.observe(function(false)).action, Some(Action::Release));
}

#[test]
fn windows_lifecycle_interruption_waits_for_chord_release() {
    let (sender, _events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let mut policy = Windows::default();
    policy.observe(LEFT_CONTROL, true, NONE_HELD);
    assert!(
        policy
            .observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]))
            .starts()
    );
    policy.interrupt(&input);
    assert_eq!(
        policy
            .observe(RIGHT_WINDOWS, true, held(&[LEFT_CONTROL, LEFT_WINDOWS]))
            .action,
        None
    );
    assert_eq!(policy.observe(RIGHT_WINDOWS, false, NONE_HELD).action, None);
    policy.observe(RIGHT_CONTROL, true, NONE_HELD);
    assert!(
        policy
            .observe(RIGHT_WINDOWS, true, held(&[RIGHT_CONTROL]))
            .starts()
    );
}

#[test]
fn windows_missed_release_and_other_shortcuts_do_not_restart_hold() {
    let mut policy = Windows::default();
    policy.observe(LEFT_CONTROL, true, NONE_HELD);
    assert!(
        policy
            .observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]))
            .starts()
    );
    let both = held(&[LEFT_CONTROL, LEFT_WINDOWS]);
    assert_eq!(
        policy.observe(LETTER_A, true, both).action,
        Some(Action::Interrupt)
    );
    assert!(!policy.observe(RIGHT_WINDOWS, true, both).starts());
    assert_eq!(policy.observe(RIGHT_WINDOWS, false, NONE_HELD).action, None);
    assert_eq!(policy.observe(RIGHT_CONTROL, true, NONE_HELD).action, None);
    assert!(
        policy
            .observe(RIGHT_WINDOWS, true, held(&[RIGHT_CONTROL]))
            .starts()
    );
    assert_eq!(
        policy.observe(LEFT_CONTROL, true, NONE_HELD).action,
        Some(Action::Release)
    );
}

#[test]
fn mac_function_space_and_escape_are_observed_without_native_events() {
    let mut policy = Mac::default();
    assert!(policy.observe(function(true)).starts());
    assert_eq!(policy.observe(function(true)).action, None);
    assert!(policy.observe(key(Mac::SPACE, true)).swallows());
    assert_eq!(
        policy.observe(function(false)).action,
        Some(Action::Release)
    );
    assert!(policy.observe(key(Mac::SPACE, false)).swallows());
    let escape = policy.observe(key(Mac::ESCAPE, true));
    assert_eq!(escape.action, Some(Action::Cancel));
    assert!(!escape.swallows());
    assert!(policy.observe(function(true)).starts());
    assert_eq!(policy.observe(key(0, true)).action, Some(Action::Interrupt));
    assert!(!policy.observe(function(true)).starts());
    policy.observe(function(false));
    assert!(policy.observe(function(true)).starts());
}

#[test]
fn hands_free_space_press_and_release_never_reach_the_app() {
    let mut chord = Chord::default();
    assert_eq!(chord.press(), Some(Action::Press));
    assert!(chord.space(true).swallows());
    assert!(chord.space(false).swallows());
    assert!(!chord.space(false).swallows());
    assert_eq!(chord.release(), Some(Action::Release));
    assert!(
        !chord.space(true).swallows(),
        "Space was swallowed without a hold"
    );
}

#[test]
fn hands_free_space_stays_swallowed_until_its_release() {
    let mut chord = Chord::default();
    assert_eq!(chord.press(), Some(Action::Press));
    assert!(chord.space(true).swallows());
    assert!(
        chord.space(true).swallows(),
        "A held shortcut must swallow Space repeats"
    );
    assert_eq!(chord.release(), Some(Action::Release));
    assert!(
        chord.space(true).swallows(),
        "Releasing the shortcut must not leak a held Space into the editor"
    );
    assert!(chord.space(false).swallows());
    assert!(
        !chord.space(true).swallows(),
        "A fresh Space belongs to the editor"
    );
    assert!(!chord.space(false).swallows());
}
