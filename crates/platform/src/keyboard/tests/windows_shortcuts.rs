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

fn f13_held_since(at: impl Fn(u64) -> Instant, repeats: &[u64]) -> Windows {
    let mut policy = Windows::new(WindowsShortcut::F13);
    assert!(policy.observe(F13, true, NONE_HELD, at(0)).starts());
    for &millis in repeats {
        let repeat = policy.observe(F13, true, NONE_HELD, at(millis));
        assert_eq!(delivered(&repeat), [], "An F13 repeat at {millis} ms acted");
    }
    policy
}

#[test]
fn a_hold_key_after_f13_falls_quiet_ends_the_hold_instead_of_acting() {
    let start = now();
    let at = |millis| start + Duration::from_millis(millis);
    for (repeats, typed_at) in [(&[][..], 5_000), (&[500, 533, 566][..], 2_200)] {
        let mut policy = f13_held_since(at, repeats);
        let typed = policy.observe(LETTER_S, true, NONE_HELD, at(typed_at));
        assert_eq!(
            delivered(&typed),
            [Action::Release],
            "S at {typed_at} ms after F13 {repeats:?} with no sign of F13 since"
        );
        assert!(!typed.swallows());
    }

    let mut policy = f13_held_since(at, &[500, 533]);
    policy.observe(LEFT_SHIFT, true, NONE_HELD, at(600));
    let typed = policy.observe(LETTER_S, true, NONE_HELD, at(2_500));
    assert_eq!(
        delivered(&typed),
        [Action::Release],
        "Shift stopped F13's repeats, so its release could have gone unseen"
    );
    assert!(!typed.swallows());

    let steady: Vec<u64> = (0..300).map(|n| 500 + n * 33).collect();
    for (repeats, show_at) in [
        (&[][..], 1_200),
        (&[500, 533, 566][..], 1_900),
        (&steady[..], 10_400),
    ] {
        let mut policy = f13_held_since(at, repeats);
        let show = policy.observe(LETTER_S, true, NONE_HELD, at(show_at));
        assert_eq!(
            delivered(&show),
            [Action::Show(Scope::Screen)],
            "S at {show_at} ms during a live F13 hold"
        );
        assert!(show.swallows());
    }
}

#[test]
fn slow_windows_f13_repeats_never_split_a_dictation() {
    let start = now();
    let at = |millis| start + Duration::from_millis(millis);
    let mut policy = f13_held_since(at, &[3_000, 6_000, 20_000]);
    let show = policy.observe(LETTER_S, true, NONE_HELD, at(20_500));
    assert_eq!(delivered(&show), [Action::Show(Scope::Screen)]);
    assert_eq!(
        delivered(&policy.observe(F13, false, NONE_HELD, at(21_000))),
        [Action::Release]
    );
}

#[test]
fn a_windows_f13_hold_whose_release_went_unseen_ends_at_the_next_f13_release() {
    let start = now();
    let at = |millis| start + Duration::from_millis(millis);
    let mut policy = f13_held_since(at, &[5_000]);
    let later = at(6_000);
    assert_eq!(
        delivered(&policy.observe(F13, false, NONE_HELD, later)),
        [Action::Release]
    );
    assert!(policy.observe(F13, true, NONE_HELD, later).starts());

    let (sender, _events) = async_channel::bounded(4);
    let input = InputSender::new(sender);
    let mut policy = f13_held_since(at, &[]);
    policy.interrupt(&input);
    assert_eq!(
        delivered(&policy.observe(F13, true, NONE_HELD, at(500))),
        [Action::Press],
        "The first F13 press after a lifecycle interrupt was taken as a repeat"
    );
}

#[test]
fn only_ctrl_win_masks_the_start_menu() {
    assert!(WindowsShortcut::ControlWindows.masks_start());
    assert!(!WindowsShortcut::RightControl.masks_start());
    assert!(!WindowsShortcut::F13.masks_start());
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
