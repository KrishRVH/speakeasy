//! Pure keyboard policy for the modifier-only shortcuts: Ctrl+Win on Windows, Fn on macOS, and
//! the held-modifier rules for paste and Show ink.
//!
//! Native callbacks report key events and deliver the decided input. Modifiers and Escape stay
//! passive; only Space for hands-free and S for a Show are swallowed, and only when pressed during
//! a hold. Shift never interrupts a hold, since a Show draws with it.

use std::mem;

use super::{Input, InputSender};

/// Hold state for the modifier-only shortcut.
///
/// Space during a hold locks hands-free, and S turns the recording into a Show; any other key
/// belongs to a different shortcut, so it interrupts dictation and waits for the chord's release.
#[derive(Default)]
struct Chord {
    hold: Hold,
    swallowing_space: bool,
    swallowing_show: bool,
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
        swallow_while_held(self.hold, &mut self.swallowing_space, down, Action::Lock)
    }

    /// Swallows S pressed during a hold the way [`space`](Self::space) swallows Space.
    fn show(&mut self, down: bool) -> Decision {
        swallow_while_held(self.hold, &mut self.swallowing_show, down, Action::Show)
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

/// Swallows a key pressed while the chord is held, from that press through its release, and
/// delivers `action` on each press.
fn swallow_while_held(hold: Hold, swallowing: &mut bool, down: bool, action: Action) -> Decision {
    let swallowed = if down {
        *swallowing |= hold == Hold::Held;
        *swallowing
    } else {
        mem::take(swallowing)
    };
    if swallowed {
        Decision::swallow(down.then_some(action))
    } else {
        Decision::pass(None)
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
    Show,
    Cancel,
    /// Interrupt, then release: another shortcut or a session change ended the hold.
    Interrupt,
}

impl Action {
    fn deliver(self, input: &InputSender) {
        match self {
            Self::Press => input.deliver(Input::Press),
            Self::Release => input.deliver(Input::Release),
            Self::Lock => input.deliver(Input::Lock),
            Self::Show => input.deliver(Input::Show),
            Self::Cancel => input.deliver(Input::Cancel),
            Self::Interrupt => {
                input.deliver(Input::Interrupt);
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

/// Modifier keys held right now, as the paste wait and Show ink read them.
#[derive(Clone, Copy, Default)]
pub(super) struct Modifiers {
    pub(super) shift: bool,
    /// Any other held combination, including part of the talk shortcut alone, which makes Shift
    /// part of another shortcut.
    pub(super) shortcut: bool,
    /// The hold-to-talk shortcut, macOS Fn or Windows Ctrl+Win, which may be down while Shift
    /// draws.
    pub(super) talk: bool,
}

impl Modifiers {
    /// Insertion waits while any modifier is held, so a paste never merges with the shortcut.
    pub(super) const fn any(self) -> bool {
        self.shift || self.shortcut || self.talk
    }

    /// Shift alone draws, so Shift shortcuts never draw; the talk shortcut does not count, since
    /// the user may still be holding it after pressing S.
    pub(super) const fn draws(self) -> bool {
        self.shift && !self.shortcut
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
    const ALT: u16 = 0x12;
    /// Left Ctrl, right Ctrl, left Windows, and right Windows; either Ctrl with either Windows key
    /// holds the chord.
    pub(super) const CHORD_KEYS: [u16; 4] = [0xA2, 0xA3, Self::LEFT_WINDOWS, Self::RIGHT_WINDOWS];
    const CONTROL: u16 = 0x11;
    const ESCAPE: u16 = 0x1B;
    const LEFT_WINDOWS: u16 = 0x5B;
    const RIGHT_WINDOWS: u16 = 0x5C;
    const SHIFT: u16 = 0x10;
    /// Shift, left Shift, and right Shift. macOS reports them as flag changes, which never
    /// interrupt either.
    const SHIFT_KEYS: [u16; 3] = [Self::SHIFT, 0xA0, 0xA1];
    const SHOW: u16 = 0x53;
    const SPACE: u16 = 0x20;

    /// The modifiers `down` reports held, by virtual-key code. Ctrl and Windows together are the
    /// talk shortcut; either alone, or Alt, makes Shift part of another shortcut, such as
    /// Win+Shift+S.
    pub(super) fn modifiers(down: impl Fn(u16) -> bool) -> Modifiers {
        let control = down(Self::CONTROL);
        let windows = down(Self::LEFT_WINDOWS) || down(Self::RIGHT_WINDOWS);
        Modifiers {
            shift: down(Self::SHIFT),
            shortcut: down(Self::ALT) || control != windows,
            talk: control && windows,
        }
    }

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
            Ok(Self::SHOW) => self.chord.show(down),
            Ok(key) if Self::SHIFT_KEYS.contains(&key) => Decision::pass(None),
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
    const SHOW: i64 = 1;
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
                Self::SHOW => self.chord.show(down),
                _ => self.chord.other_key(down),
            },
        }
    }
}

#[cfg(test)]
mod tests;
