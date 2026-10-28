//! Pure keyboard policy for the modifier-only shortcuts: Ctrl+Win on Windows, Fn on macOS, the
//! held-modifier rules for paste and Show ink, and the Shift double tap that drops a pin.
//!
//! Native callbacks report key events and deliver the decided input. Modifiers and Escape stay
//! passive; only Space for hands-free and S, W, and A for a Show are swallowed, and only when
//! pressed during a hold. Shift never interrupts a hold, since a Show draws with it.

use std::{
    mem,
    time::{Duration, Instant},
};

use super::{Input, InputSender, Scope, Taps};

/// A Shift press released within this long is a tap.
const SHIFT_TAP: Duration = Duration::from_millis(300);
/// A second tap begins within this long of the first tap's release.
const SHIFT_TAP_GAP: Duration = Duration::from_millis(400);

/// Hold state for the modifier-only shortcut.
///
/// Space during a hold locks hands-free, and S, W, or A turns the recording into a Show; any other
/// key belongs to a different shortcut, so it interrupts dictation and waits for the chord's
/// release.
#[derive(Default)]
struct Chord {
    hold: Hold,
    /// The [`HoldKey`]s being swallowed, by [`HoldKey::bit`].
    swallowing: u8,
}

/// A key that acts when pressed during a hold.
#[derive(Clone, Copy)]
enum HoldKey {
    /// Space locks hands-free.
    Space,
    /// S, W, and A record a Show of the screen, the window under the pointer, or an area.
    Show(Scope),
}

impl HoldKey {
    const fn action(self) -> Action {
        match self {
            Self::Space => Action::Lock,
            Self::Show(scope) => Action::Show(scope),
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::Space => 1,
            Self::Show(Scope::Screen) => 2,
            Self::Show(Scope::Window) => 4,
            Self::Show(Scope::Area) => 8,
        }
    }
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

    /// Swallows `key` pressed during a hold through its release, including repeats after the
    /// chord itself is released, and acts on each press.
    fn hold_key(&mut self, key: HoldKey, down: bool) -> Decision {
        let bit = key.bit();
        if down && self.hold == Hold::Held {
            self.swallowing |= bit;
        }
        let swallowed = self.swallowing & bit != 0;
        if !down {
            self.swallowing &= !bit;
        }
        if swallowed {
            Decision::swallow(down.then_some(key.action()))
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

/// Two quick taps of Shift alone, which drop a numbered pin during a Show.
///
/// A tap is a press released within [`SHIFT_TAP`], and the second begins within [`SHIFT_TAP_GAP`]
/// of the first's release with no other key between or held. The talk shortcut held through both
/// taps does not count as another key. Shift stays passive, so an app's own double Shift still
/// works.
///
/// Every tap also begins the next pair, and the Show decides which pairs pin, since only it sees
/// whether a tap drew ink: a quick stroke before a double tap then costs that double tap nothing.
#[derive(Clone, Copy, Default)]
enum ShiftTaps {
    #[default]
    Idle,
    /// Shift went down at `at`, as the second tap of a pair begun at `first` when there is one.
    Down { at: Instant, first: Option<Instant> },
    /// A tap pressed at `pressed` ended at `released`.
    Tapped { pressed: Instant, released: Instant },
}

impl ShiftTaps {
    /// Observes Shift going down or up at `at`; returns the double tap that completed, if any.
    fn shift(&mut self, down: bool, at: Instant) -> Option<Taps> {
        let (next, doubled) = match (*self, down) {
            // Autorepeat while Shift is held.
            (Self::Down { .. }, true) => (*self, None),
            (Self::Tapped { pressed, released }, true) => {
                let first =
                    (at.saturating_duration_since(released) <= SHIFT_TAP_GAP).then_some(pressed);
                (Self::Down { at, first }, None)
            },
            (Self::Idle, true) => (Self::Down { at, first: None }, None),
            (Self::Down { at: pressed, first }, false) => {
                if at.saturating_duration_since(pressed) <= SHIFT_TAP {
                    let doubled = first.map(|first| Taps {
                        pressed: first,
                        released: at,
                    });
                    (
                        Self::Tapped {
                            pressed,
                            released: at,
                        },
                        doubled,
                    )
                } else {
                    (Self::Idle, None)
                }
            },
            (Self::Idle | Self::Tapped { .. }, false) => (Self::Idle, None),
        };
        *self = next;
        doubled
    }

    /// Another key came between taps.
    fn interrupt(&mut self) {
        *self = Self::Idle;
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
    Show(Scope),
    Pin(Taps),
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
            Self::Show(scope) => input.deliver(Input::Show(scope)),
            Self::Pin(taps) => input.deliver(Input::Pin(taps)),
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

/// The keys Windows reports physically held just before a hook event, which corrects for events
/// the hook missed, such as releases on the secure desktop.
#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy, Default)]
pub(super) struct Physical {
    /// Each of [`Windows::CHORD_KEYS`].
    pub(super) chord_keys: [bool; 4],
    /// [`Modifiers::shortcut`]: Shift pressed now belongs to another shortcut.
    pub(super) shortcut: bool,
}

/// Ctrl+Win policy over virtual-key codes from the low-level keyboard hook.
#[cfg(any(target_os = "windows", test))]
#[derive(Default)]
pub(super) struct Windows {
    chord: Chord,
    chord_keys_down: [bool; 4],
    taps: ShiftTaps,
}

#[cfg(any(target_os = "windows", test))]
impl Windows {
    const ALT: u16 = 0x12;
    const AREA: u16 = 0x41;
    /// Left Ctrl, right Ctrl, left Windows, and right Windows; either Ctrl with either Windows key
    /// holds the chord.
    pub(super) const CHORD_KEYS: [u16; 4] = [0xA2, 0xA3, Self::LEFT_WINDOWS, Self::RIGHT_WINDOWS];
    const CONTROL: u16 = 0x11;
    const ESCAPE: u16 = 0x1B;
    const LEFT_WINDOWS: u16 = 0x5B;
    const RIGHT_WINDOWS: u16 = 0x5C;
    const SCREEN: u16 = 0x53;
    const SHIFT: u16 = 0x10;
    /// Shift, left Shift, and right Shift. macOS reports them as flag changes, which never
    /// interrupt either.
    const SHIFT_KEYS: [u16; 3] = [Self::SHIFT, 0xA0, 0xA1];
    const SPACE: u16 = 0x20;
    const WINDOW: u16 = 0x57;

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

    /// Decides one hook event at `at`. `physical` is what the OS reports held before this event;
    /// it clears chord keys whose release the hook missed and tells Shift alone from Shift in
    /// another shortcut.
    pub(super) fn observe(
        &mut self,
        key: u32,
        down: bool,
        physical: Physical,
        at: Instant,
    ) -> Decision {
        let key = u16::try_from(key).unwrap_or(u16::MAX);
        if Self::SHIFT_KEYS.contains(&key) {
            let pinned = if physical.shortcut {
                self.taps.interrupt();
                None
            } else {
                self.taps.shift(down, at)
            };
            return Decision::pass(pinned.map(Action::Pin));
        }
        if Self::CHORD_KEYS.contains(&key) {
            return self.chord_key(key, down, physical.chord_keys);
        }
        self.taps.interrupt();
        match key {
            Self::ESCAPE => Decision::pass(down.then_some(Action::Cancel)),
            Self::SPACE => self.chord.hold_key(HoldKey::Space, down),
            Self::SCREEN => self.chord.hold_key(HoldKey::Show(Scope::Screen), down),
            Self::WINDOW => self.chord.hold_key(HoldKey::Show(Scope::Window), down),
            Self::AREA => self.chord.hold_key(HoldKey::Show(Scope::Area), down),
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
                if *tracked != down {
                    // Ctrl or Win pressed or released between taps makes a shortcut such as
                    // Ctrl+Shift; holding the talk shortcut through both taps does not.
                    self.taps.interrupt();
                }
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
    /// Modifier flags changed, leaving `held` held.
    FlagsChanged { code: i64, held: Modifiers },
}

/// Fn policy over virtual key codes from the macOS event tap.
#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
pub(super) struct Mac {
    chord: Chord,
    taps: ShiftTaps,
}

#[cfg(any(target_os = "macos", test))]
impl Mac {
    const AREA: i64 = 0;
    const ESCAPE: i64 = 53;
    const FUNCTION: i64 = 63;
    const LEFT_SHIFT: i64 = 56;
    const RIGHT_SHIFT: i64 = 60;
    const SCREEN: i64 = 1;
    const SPACE: i64 = 49;
    const WINDOW: i64 = 13;

    /// Decides one event-tap event at `at`.
    pub(super) fn observe(&mut self, event: MacEvent, at: Instant) -> Decision {
        match event {
            MacEvent::FlagsChanged {
                code: Self::FUNCTION,
                held,
            } => Decision::pass(if held.talk {
                self.chord.press()
            } else {
                self.chord.release()
            }),
            MacEvent::FlagsChanged {
                code: Self::LEFT_SHIFT | Self::RIGHT_SHIFT,
                held,
            } => {
                let pinned = if held.shortcut {
                    self.taps.interrupt();
                    None
                } else {
                    self.taps.shift(held.shift, at)
                };
                Decision::pass(pinned.map(Action::Pin))
            },
            MacEvent::FlagsChanged { .. } => {
                self.taps.interrupt();
                Decision::pass(None)
            },
            MacEvent::Key { code, down } => {
                self.taps.interrupt();
                match code {
                    Self::ESCAPE => Decision::pass(down.then_some(Action::Cancel)),
                    Self::SPACE => self.chord.hold_key(HoldKey::Space, down),
                    Self::SCREEN => self.chord.hold_key(HoldKey::Show(Scope::Screen), down),
                    Self::WINDOW => self.chord.hold_key(HoldKey::Show(Scope::Window), down),
                    Self::AREA => self.chord.hold_key(HoldKey::Show(Scope::Area), down),
                    _ => self.chord.other_key(down),
                }
            },
        }
    }
}

#[cfg(test)]
mod tests;
