//! Pure keyboard policy for the modifier-only shortcuts: Ctrl+Win on Windows, Fn on macOS.
//!
//! Native callbacks report key events and deliver the decided input. Modifiers and Escape stay
//! passive; only Space used for hands-free is swallowed.

use std::mem;

use super::{Input, InputSender};

/// Hold state for the modifier-only shortcut.
///
/// Space during a hold locks hands-free; any other key belongs to a different shortcut, so it
/// cancels dictation and waits for the chord's release.
#[derive(Default)]
struct Chord {
    hold: Hold,
    swallowing_space: bool,
}

impl Chord {
    /// Starts a hold when the chord completes, unless it is already held or interrupted.
    fn press(&mut self) -> Option<Action> {
        if self.hold != Hold::Released {
            return None;
        }
        self.hold = Hold::Held;
        Some(Action::Press)
    }

    /// Ends the hold when the chord breaks; only an uninterrupted hold reports a release.
    fn release(&mut self) -> Option<Action> {
        (mem::take(&mut self.hold) == Hold::Held).then_some(Action::Release)
    }

    /// Swallows Space pressed during a hold through its release, including repeats after the chord
    /// itself is released.
    fn space(&mut self, down: bool) -> Decision {
        let swallowed = if down {
            self.swallowing_space |= self.hold == Hold::Held;
            self.swallowing_space
        } else {
            mem::take(&mut self.swallowing_space)
        };
        if swallowed {
            Decision::swallow(down.then_some(Action::Lock))
        } else {
            Decision::pass(None)
        }
    }

    fn other_key(&mut self, down: bool) -> Decision {
        let interrupted = down && self.interrupt();
        Decision::pass(interrupted.then_some(Action::Interrupt))
    }

    /// Ends an active hold; returns whether there was one.
    fn interrupt(&mut self) -> bool {
        let held = self.hold == Hold::Held;
        if held {
            self.hold = Hold::Interrupted;
        }
        held
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Hold {
    #[default]
    Released,
    Held,
    /// Another key ended the hold; nothing restarts it until the chord is released.
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Press,
    Release,
    Lock,
    Cancel,
    /// Cancel, then release: another shortcut or a session change ended the hold.
    Interrupt,
}

impl Action {
    fn deliver(self, input: &InputSender) {
        match self {
            Self::Press => input.deliver(Input::Press),
            Self::Release => input.deliver(Input::Release),
            Self::Lock => input.deliver(Input::Lock),
            Self::Cancel => input.deliver(Input::Cancel),
            Self::Interrupt => {
                input.deliver(Input::Cancel);
                input.deliver(Input::Release);
            },
        }
    }
}

/// What a native callback does with one key event.
pub(super) struct Decision {
    action: Option<Action>,
    swallowed: bool,
}

impl Decision {
    const fn pass(action: Option<Action>) -> Self {
        Self {
            action,
            swallowed: false,
        }
    }

    const fn swallow(action: Option<Action>) -> Self {
        Self {
            action,
            swallowed: true,
        }
    }

    /// Whether the focused app must not receive the event.
    pub(super) const fn swallows(&self) -> bool {
        self.swallowed
    }

    /// Whether the event starts a hold.
    #[cfg(any(target_os = "windows", test))]
    pub(super) fn starts(&self) -> bool {
        self.action == Some(Action::Press)
    }

    pub(super) fn deliver(&self, input: &InputSender) {
        if let Some(action) = self.action {
            action.deliver(input);
        }
    }
}

/// Ctrl+Win policy over virtual-key codes from the low-level keyboard hook.
#[cfg(any(target_os = "windows", test))]
#[derive(Default)]
pub(super) struct Windows {
    chord: Chord,
    chord_keys_down: [bool; 4],
}

#[cfg(any(target_os = "windows", test))]
impl Windows {
    /// Left Ctrl, right Ctrl, left Windows, and right Windows; either Ctrl with either Windows key
    /// holds the chord.
    pub(super) const CHORD_KEYS: [u16; 4] = [0xA2, 0xA3, 0x5B, 0x5C];
    const ESCAPE: u16 = 0x1B;
    const SPACE: u16 = 0x20;

    /// Ends dictation for a session change, then waits for the chord's release.
    pub(super) fn interrupt(&mut self, input: &InputSender) {
        Action::Interrupt.deliver(input);
        self.chord.interrupt();
    }

    /// Decides one hook event. `physical_before_event` is the OS state of [`Self::CHORD_KEYS`]
    /// before this event; it clears chord keys whose release the hook missed.
    pub(super) fn observe(
        &mut self,
        key: u32,
        down: bool,
        physical_before_event: [bool; 4],
    ) -> Decision {
        match u16::try_from(key) {
            Ok(Self::ESCAPE) => Decision::pass(down.then_some(Action::Cancel)),
            Ok(Self::SPACE) => self.chord.space(down),
            Ok(key) if Self::CHORD_KEYS.contains(&key) => {
                self.chord_key(key, down, physical_before_event)
            },
            _ => self.chord.other_key(down),
        }
    }

    fn chord_key(&mut self, key: u16, down: bool, physical_before_event: [bool; 4]) -> Decision {
        let mut fresh = false;
        for ((&chord_key, tracked), physically_down) in Self::CHORD_KEYS
            .iter()
            .zip(&mut self.chord_keys_down)
            .zip(physical_before_event)
        {
            if chord_key == key {
                fresh = down && !*tracked;
                *tracked = down;
            } else if !physically_down {
                *tracked = false;
            }
        }
        let [left_control, right_control, left_windows, right_windows] = self.chord_keys_down;
        let complete = (left_control || right_control) && (left_windows || right_windows);
        Decision::pass(match (complete, fresh) {
            (false, _) => self.chord.release(),
            (true, true) => self.chord.press(),
            (true, false) => None,
        })
    }
}

/// One event from the macOS keyboard event tap.
#[cfg(any(target_os = "macos", test))]
pub(super) enum MacEvent {
    /// A key went down, including autorepeat, or up.
    Key { code: i64, down: bool },
    /// Modifier flags changed; `function` reports whether Fn is now held.
    FlagsChanged { code: i64, function: bool },
}

/// Fn policy over virtual key codes from the macOS event tap.
#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
pub(super) struct Mac {
    chord: Chord,
}

#[cfg(any(target_os = "macos", test))]
impl Mac {
    const ESCAPE: i64 = 53;
    const FUNCTION: i64 = 63;
    const SPACE: i64 = 49;

    /// Decides one event-tap event.
    pub(super) fn observe(&mut self, event: MacEvent) -> Decision {
        match event {
            MacEvent::FlagsChanged { code, function } => Decision::pass(match (code, function) {
                (Self::FUNCTION, true) => self.chord.press(),
                (Self::FUNCTION, false) => self.chord.release(),
                _ => None,
            }),
            MacEvent::Key { code, down } => match code {
                Self::ESCAPE => Decision::pass(down.then_some(Action::Cancel)),
                Self::SPACE => self.chord.space(down),
                _ => self.chord.other_key(down),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    const LEFT_CONTROL: u32 = 0xA2;
    const RIGHT_CONTROL: u32 = 0xA3;
    const LEFT_WINDOWS: u32 = 0x5B;
    const RIGHT_WINDOWS: u32 = 0x5C;
    const ESCAPE: u32 = 0x1B;
    const SPACE: u32 = 0x20;
    const LETTER_A: u32 = 0x41;
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
    fn chord_ignores_a_second_press_while_held() {
        let mut chord = Chord::default();
        assert_eq!(chord.press(), Some(Action::Press));
        assert_eq!(chord.press(), None, "A second press restarted the hold");
        assert_eq!(chord.release(), Some(Action::Release));
    }

    #[test]
    fn chord_interrupted_by_another_shortcut_waits_for_release() {
        let mut chord = Chord::default();
        assert_eq!(chord.press(), Some(Action::Press));
        assert!(chord.interrupt());
        assert_eq!(chord.press(), None, "Interrupted chord restarted");
        assert_eq!(chord.release(), None);
        assert_eq!(chord.press(), Some(Action::Press));
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
}
