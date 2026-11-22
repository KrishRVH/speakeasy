use super::*;

const F13: u32 = 0x7C;
const LETTER_C: u32 = 0x43;

fn right_control_holding(at: Instant) -> Windows {
    let mut policy = Windows::new(WindowsShortcut::RightControl);
    assert!(policy.observe(RIGHT_CONTROL, true, NONE_HELD, at).starts());
    policy
}

#[test]
fn right_control_dictates_alone_and_leaves_quick_combinations_to_the_app() {
    let pressed = now();
    let held = held_for(WindowsShortcut::RightControl, &[RIGHT_CONTROL]);

    let mut policy = right_control_holding(pressed);
    let copy = policy.observe(LETTER_C, true, held, pressed);
    assert_eq!(copy.action, Some(Action::Interrupt));
    assert!(!copy.swallows(), "Right Ctrl+C must still copy");

    let mut policy = right_control_holding(pressed);
    let early = policy.observe(SPACE, true, held, pressed + Duration::from_millis(100));
    assert!(
        !early.swallows(),
        "A quick Right Ctrl+Space belongs to the app"
    );

    let mut policy = right_control_holding(pressed);
    let later = pressed + APPLICATION_SHORTCUT;
    let lock = policy.observe(SPACE, true, held, later);
    assert_eq!(lock.action, Some(Action::Lock));
    assert!(lock.swallows());
    let release = policy.observe(RIGHT_CONTROL, false, held, later);
    assert_eq!(release.action, Some(Action::Release));
}

#[test]
fn right_control_ignores_the_left_control_and_win_keys_of_other_shortcuts() {
    let mut policy = Windows::new(WindowsShortcut::RightControl);
    assert_eq!(
        policy.observe(LEFT_CONTROL, true, NONE_HELD, now()).action,
        None
    );
    let both = held_for(WindowsShortcut::RightControl, &[LEFT_CONTROL]);
    assert!(
        !policy.observe(RIGHT_CONTROL, true, both, now()).starts(),
        "Ctrl+Right Ctrl is another shortcut"
    );

    let mut policy = Windows::new(WindowsShortcut::RightControl);
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    assert!(
        !policy
            .observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), now())
            .starts(),
        "Ctrl+Win no longer dictates once Right Ctrl is chosen"
    );
}

#[test]
fn windows_f13_is_reserved_and_its_repeats_never_restart() {
    let mut policy = Windows::new(WindowsShortcut::F13);
    let press = policy.observe(F13, true, NONE_HELD, now());
    assert!(press.starts());
    assert!(press.swallows(), "F13 must not reach the focused app");
    let down = held_for(WindowsShortcut::F13, &[]);
    let repeat = policy.observe(F13, true, down, now());
    assert_eq!(repeat.action, None);
    assert!(repeat.swallows());
    let release = policy.observe(F13, false, down, now());
    assert_eq!(release.action, Some(Action::Release));
    assert!(release.swallows());

    let mut policy = Windows::new(WindowsShortcut::F13);
    policy.observe(LEFT_CONTROL, true, NONE_HELD, now());
    assert!(
        !policy
            .observe(LEFT_WINDOWS, true, held(&[LEFT_CONTROL]), now())
            .starts()
    );
}

#[test]
fn the_chosen_windows_key_holds_the_paste_and_shift_still_draws_beside_it() {
    let read = |shortcut: WindowsShortcut, keys: &[u32]| {
        let control = keys.contains(&LEFT_CONTROL) || keys.contains(&RIGHT_CONTROL);
        shortcut.modifiers(|key| {
            let key = u32::from(key);
            keys.contains(&key) || (key == CONTROL && control)
        })
    };
    assert!(read(WindowsShortcut::RightControl, &[RIGHT_CONTROL]).any());
    assert!(read(WindowsShortcut::RightControl, &[SHIFT, RIGHT_CONTROL]).draws());
    assert!(!read(WindowsShortcut::RightControl, &[SHIFT, LEFT_CONTROL]).draws());
    assert!(!read(WindowsShortcut::F13, &[F13]).any());
    assert!(read(WindowsShortcut::F13, &[SHIFT, F13]).draws());
    assert!(!read(WindowsShortcut::F13, &[SHIFT, CONTROL]).draws());
}
