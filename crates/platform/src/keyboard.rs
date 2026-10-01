//! Pure keyboard policy. Native callbacks supply observed keys and modifiers;
//! only Space used for hands-free is swallowed. Escape remains passive.
use super::{Chord, Input, InputSender, deliver};

#[derive(Debug, PartialEq, Eq)]
enum Action {
    None,
    Press,
    Release,
    Lock,
    Cancel,
    Interrupt,
}
pub(super) struct Decision {
    action: Action,
    pub swallow: bool,
}
impl Decision {
    fn new(action: Action, swallow: bool) -> Self {
        Self { action, swallow }
    }
    #[cfg(any(target_os = "windows", test))]
    pub fn starts(&self) -> bool {
        self.action == Action::Press
    }
    pub fn deliver(&self, input: &InputSender) {
        match self.action {
            Action::None => {}
            Action::Press => deliver(input, Input::Press),
            Action::Release => deliver(input, Input::Release),
            Action::Lock => deliver(input, Input::Lock),
            Action::Cancel => deliver(input, Input::Cancel),
            Action::Interrupt => {
                deliver(input, Input::Cancel);
                deliver(input, Input::Release);
            }
        }
    }
}
#[cfg(any(target_os = "windows", test))]
#[derive(Default)]
pub(super) struct Windows {
    chord: Chord,
    down: [bool; 4],
}
#[cfg(any(target_os = "windows", test))]
impl Windows {
    pub fn interrupt(&mut self) {
        self.chord.interrupt();
    }
    pub fn observe(&mut self, key: u32, down: bool, physical: [bool; 4]) -> Decision {
        if key == 0x1b {
            return Decision::new(if down { Action::Cancel } else { Action::None }, false);
        }
        if key == 0x20 && self.chord.space(down) {
            return Decision::new(if down { Action::Lock } else { Action::None }, true);
        }
        let Some(index) = [0xa2, 0xa3, 0x5b, 0x5c].iter().position(|&vk| vk == key) else {
            return Decision::new(
                if down && self.chord.interrupt() {
                    Action::Interrupt
                } else {
                    Action::None
                },
                false,
            );
        };
        let fresh = down && !self.down[index];
        self.down[index] = down;
        for (other, held) in physical.into_iter().enumerate() {
            if other != index && !held {
                self.down[other] = false;
            }
        }
        let held = (self.down[0] || self.down[1]) && (self.down[2] || self.down[3]);
        Decision::new(
            match self.chord.modifiers(held, fresh) {
                Some(Input::Press) => Action::Press,
                Some(Input::Release) => Action::Release,
                _ => Action::None,
            },
            false,
        )
    }
}
#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
pub(super) struct Mac {
    chord: Chord,
}
#[cfg(any(target_os = "macos", test))]
impl Mac {
    pub fn observe(&mut self, key: i64, down: bool, modifiers: bool, function: bool) -> Decision {
        if modifiers {
            return Decision::new(
                if key == 63 {
                    match self.chord.modifiers(function, function) {
                        Some(Input::Press) => Action::Press,
                        Some(Input::Release) => Action::Release,
                        _ => Action::None,
                    }
                } else {
                    Action::None
                },
                false,
            );
        }
        if key == 53 {
            return Decision::new(if down { Action::Cancel } else { Action::None }, false);
        }
        if key == 49 && self.chord.space(down) {
            return Decision::new(if down { Action::Lock } else { Action::None }, true);
        }
        Decision::new(
            if down && self.chord.interrupt() {
                Action::Interrupt
            } else {
                Action::None
            },
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interruption_revokes_authority_before_delivering_release() -> anyhow::Result<()> {
        let (sender, events) = async_channel::bounded(4);
        let input = InputSender::new(sender);
        let permit = input
            .begin()
            .ok_or_else(|| anyhow::anyhow!("Missing permit"))?;
        Decision::new(Action::Interrupt, false).deliver(&input);
        assert!(!permit.active());
        assert!(matches!(events.try_recv()?, Input::Cancel));
        assert!(matches!(events.try_recv()?, Input::Release));
        Ok(())
    }
    #[test]
    fn windows_sides_repeats_space_and_passive_escape() {
        let mut policy = Windows::default();
        assert_eq!(policy.observe(0xa2, true, [false; 4]).action, Action::None);
        assert!(
            policy
                .observe(0x5c, true, [true, false, false, false])
                .starts()
        );
        assert_eq!(
            policy
                .observe(0x5c, true, [true, false, false, true])
                .action,
            Action::None
        );
        assert!(
            policy
                .observe(0x20, true, [true, false, false, true])
                .swallow
        );
        assert_eq!(
            policy
                .observe(0xa2, false, [true, false, false, true])
                .action,
            Action::Release
        );
        assert!(policy.observe(0x20, false, [false; 4]).swallow);
        assert!(!policy.observe(0x1b, true, [false; 4]).swallow);
        assert_eq!(
            policy.observe(0x1b, true, [false; 4]).action,
            Action::Cancel
        );
    }
    #[test]
    fn windows_lifecycle_interruption_waits_for_chord_release() {
        let mut policy = Windows::default();
        policy.observe(0xa2, true, [false; 4]);
        assert!(
            policy
                .observe(0x5b, true, [true, false, false, false])
                .starts()
        );
        policy.interrupt();
        assert_eq!(
            policy
                .observe(0x5c, true, [true, false, true, false])
                .action,
            Action::None
        );
        assert_eq!(policy.observe(0x5c, false, [false; 4]).action, Action::None);
        policy.observe(0xa3, true, [false; 4]);
        assert!(
            policy
                .observe(0x5c, true, [false, true, false, false])
                .starts()
        );
    }
    #[test]
    fn windows_missed_release_and_other_shortcuts_do_not_restart_hold() {
        let mut policy = Windows::default();
        policy.observe(0xa2, true, [false; 4]);
        assert!(
            policy
                .observe(0x5b, true, [true, false, false, false])
                .starts()
        );
        assert_eq!(
            policy
                .observe(0x41, true, [true, false, true, false])
                .action,
            Action::Interrupt
        );
        assert!(
            !policy
                .observe(0x5c, true, [true, false, true, false])
                .starts()
        );
        assert_eq!(policy.observe(0x5c, false, [false; 4]).action, Action::None);
        assert_eq!(policy.observe(0xa3, true, [false; 4]).action, Action::None);
        assert!(
            policy
                .observe(0x5c, true, [false, true, false, false])
                .starts()
        );
        assert_eq!(
            policy.observe(0xa2, true, [false; 4]).action,
            Action::Release
        );
    }
    #[test]
    fn mac_function_space_and_escape_are_observed_without_native_events() {
        let mut policy = Mac::default();
        assert!(policy.observe(63, false, true, true).starts());
        assert_eq!(policy.observe(63, false, true, true).action, Action::None);
        assert!(policy.observe(49, true, false, true).swallow);
        assert_eq!(
            policy.observe(63, false, true, false).action,
            Action::Release
        );
        assert!(policy.observe(49, false, false, false).swallow);
        let escape = policy.observe(53, true, false, false);
        assert_eq!(escape.action, Action::Cancel);
        assert!(!escape.swallow);
        assert!(policy.observe(63, false, true, true).starts());
        assert_eq!(
            policy.observe(0, true, false, true).action,
            Action::Interrupt
        );
        assert!(!policy.observe(63, false, true, true).starts());
        policy.observe(63, false, true, false);
        assert!(policy.observe(63, false, true, true).starts());
    }
}
