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

use crate::{Input, InputSender, Scope, Taps};

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
    /// chord itself is released, but acts only on presses during the hold, so a key whose release
    /// went unseen asks for nothing once the hold is over.
    fn hold_key(&mut self, key: HoldKey, down: bool) -> Decision {
        let bit = key.bit();
        let acts = down && self.hold == Hold::Held;
        if acts {
            self.swallowing |= bit;
        }
        let swallowed = self.swallowing & bit != 0;
        if !down {
            self.swallowing &= !bit;
        }
        if swallowed {
            Decision::swallow(acts.then_some(key.action()))
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

    /// Ends dictation for a session change, then waits for the chord's release.
    fn interrupt_dictation(&mut self, input: &InputSender) {
        Action::Interrupt.deliver(input);
        self.interrupt();
    }

    /// Whether the chord went down and has not been seen to come up.
    #[cfg(any(target_os = "macos", test))]
    fn awaits_release(&self) -> bool {
        self.hold != Hold::Released
    }

    /// Ends the hold when the chord was not `held` just before this event, so its release never
    /// arrived, as on the secure desktop or across sleep. Returns whether that ended an
    /// uninterrupted hold, whose release the owner still has to hear.
    fn reconcile(&mut self, held: bool) -> bool {
        !held && self.release().is_some()
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
    /// Shift as part of another shortcut is no tap and breaks any pair.
    fn shift(&mut self, down: bool, other_shortcut: bool, at: Instant) -> Option<Taps> {
        if other_shortcut {
            self.interrupt();
            return None;
        }
        let (next, doubled) = match (*self, down) {
            // Autorepeat while Shift is held.
            (Self::Down { .. }, true) => (*self, None),
            (Self::Tapped { pressed, released }, true) => {
                let first =
                    (at.saturating_duration_since(released) <= SHIFT_TAP_GAP).then_some(pressed);
                (Self::Down { at, first }, None)
            },
            (Self::Idle, true) => (Self::Down { at, first: None }, None),
            (Self::Down { at: pressed, first }, false)
                if at.saturating_duration_since(pressed) <= SHIFT_TAP =>
            {
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
            },
            (_, false) => (Self::Idle, None),
        };
        *self = next;
        doubled
    }

    /// Another key came between taps.
    fn interrupt(&mut self) {
        *self = Self::Idle;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Press,
    Release,
    Lock,
    Show(Scope),
    Pin(Taps),
    Cancel,
    /// Interrupt, then release: another shortcut, a session change, or a key-tap outage ended
    /// dictation.
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
    /// The hold ended before this event without its release arriving; the owner hears that
    /// release before `action`.
    missed_release: bool,
    action: Option<Action>,
    swallowed: bool,
}

impl Decision {
    const fn pass(action: Option<Action>) -> Self {
        Self {
            missed_release: false,
            action,
            swallowed: false,
        }
    }

    const fn swallow(action: Option<Action>) -> Self {
        Self {
            missed_release: false,
            action,
            swallowed: true,
        }
    }

    const fn after_missed_release(self, missed_release: bool) -> Self {
        Self {
            missed_release,
            ..self
        }
    }

    /// The actions to deliver, in order.
    fn actions(&self) -> impl Iterator<Item = Action> {
        self.missed_release
            .then_some(Action::Release)
            .into_iter()
            .chain(self.action)
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
        for action in self.actions() {
            action.deliver(input);
        }
    }
}

/// Modifier keys held, as the paste wait and Show ink read them now or a macOS key event's flags
/// report them.
#[derive(Clone, Copy)]
pub(super) struct Modifiers {
    pub(super) shift: bool,
    /// Any other held combination, which makes Shift part of another shortcut; part of the talk
    /// shortcut alone counts.
    pub(super) other_shortcut: bool,
    /// The hold-to-talk shortcut, macOS Fn or Windows Ctrl+Win, which may be down while Shift
    /// draws.
    pub(super) talk: bool,
}

impl Modifiers {
    /// Insertion waits while any modifier is held, so a paste never merges with the shortcut.
    pub(super) const fn any(self) -> bool {
        self.shift || self.other_shortcut || self.talk
    }

    /// Shift alone draws, so Shift shortcuts never draw; the talk shortcut does not count, since
    /// the user may still be holding it after pressing S.
    pub(super) const fn draws(self) -> bool {
        self.shift && !self.other_shortcut
    }
}

/// The keys Windows reports physically held just before a hook event, which corrects for events
/// the hook missed, such as releases on the secure desktop.
#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy)]
pub(super) struct Physical {
    /// Each of [`Windows::CHORD_KEYS`].
    chord_keys: [bool; 4],
    /// [`Modifiers::other_shortcut`]: Shift pressed now belongs to another shortcut.
    other_shortcut: bool,
}

#[cfg(any(target_os = "windows", test))]
impl Physical {
    /// Reads each key's state through `down`, by virtual-key code.
    pub(super) fn read(down: impl Fn(u16) -> bool) -> Self {
        Self {
            chord_keys: Windows::CHORD_KEYS.map(&down),
            other_shortcut: Windows::modifiers(down).other_shortcut,
        }
    }
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
    /// Either Ctrl with either Windows key holds the chord.
    const CHORD_KEYS: [u16; 4] = [
        Self::LEFT_CONTROL,
        Self::RIGHT_CONTROL,
        Self::LEFT_WINDOWS,
        Self::RIGHT_WINDOWS,
    ];
    const CONTROL: u16 = 0x11;
    const ESCAPE: u16 = 0x1B;
    const LEFT_CONTROL: u16 = 0xA2;
    const LEFT_SHIFT: u16 = 0xA0;
    const LEFT_WINDOWS: u16 = 0x5B;
    const RIGHT_CONTROL: u16 = 0xA3;
    const RIGHT_SHIFT: u16 = 0xA1;
    const RIGHT_WINDOWS: u16 = 0x5C;
    const SCREEN: u16 = 0x53;
    const SHIFT: u16 = 0x10;
    const SHIFT_KEYS: [u16; 3] = [Self::SHIFT, Self::LEFT_SHIFT, Self::RIGHT_SHIFT];
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
            other_shortcut: down(Self::ALT) || control != windows,
            talk: control && windows,
        }
    }

    /// Ends dictation for a session change, then waits for the chord's release.
    pub(super) fn interrupt(&mut self, input: &InputSender) {
        self.chord.interrupt_dictation(input);
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
        let missed_release = self.reconcile(physical.chord_keys);
        self.decide(key, down, physical, at)
            .after_missed_release(missed_release)
    }

    /// Forgets the chord keys Windows reports up before this event and ends a hold they no longer
    /// make.
    fn reconcile(&mut self, physical_before_event: [bool; 4]) -> bool {
        for (tracked, physically_down) in self.chord_keys_down.iter_mut().zip(physical_before_event)
        {
            *tracked &= physically_down;
        }
        self.chord.reconcile(self.chord_held())
    }

    const fn chord_held(&self) -> bool {
        let [left_control, right_control, left_windows, right_windows] = self.chord_keys_down;
        (left_control || right_control) && (left_windows || right_windows)
    }

    fn decide(&mut self, key: u32, down: bool, physical: Physical, at: Instant) -> Decision {
        let key = u16::try_from(key).unwrap_or(u16::MAX);
        if Self::SHIFT_KEYS.contains(&key) {
            let pinned = self.taps.shift(down, physical.other_shortcut, at);
            return Decision::pass(pinned.map(Action::Pin));
        }
        if let Some(decision) = self.chord_key(key, down) {
            return decision;
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

    /// Follows one of [`Self::CHORD_KEYS`] going down or up; `None` for any other key.
    fn chord_key(&mut self, key: u16, down: bool) -> Option<Decision> {
        let tracked = Self::CHORD_KEYS
            .into_iter()
            .zip(&mut self.chord_keys_down)
            .find_map(|(chord_key, tracked)| (chord_key == key).then_some(tracked))?;
        let was_down = mem::replace(tracked, down);
        if was_down != down {
            // Ctrl or Win pressed or released between taps makes a shortcut such as Ctrl+Shift;
            // holding the talk shortcut through both taps does not.
            self.taps.interrupt();
        }
        let action = match (self.chord_held(), down && !was_down) {
            (false, _) => self.chord.release(),
            (true, true) => self.chord.press(),
            (true, false) => None,
        };
        Some(Decision::pass(action))
    }
}

/// One event from the macOS keyboard event tap, with the modifiers its flags report held.
#[cfg(any(target_os = "macos", test))]
pub(super) enum MacEvent {
    /// A key went down, including autorepeat, or up.
    Key {
        code: i64,
        down: bool,
        held: Modifiers,
    },
    /// Modifier flags changed.
    FlagsChanged { code: i64, held: Modifiers },
}

#[cfg(any(target_os = "macos", test))]
impl MacEvent {
    /// Whether Fn was held just before this event. Fn's own flag change reports its new state, so
    /// Fn was in the other state before. Another process may post any other event with whatever
    /// flags it likes, so flags showing Fn up count only once `keyboard_fn` agrees.
    fn fn_held_before(&self, keyboard_fn: impl FnOnce() -> bool) -> bool {
        match *self {
            Self::FlagsChanged {
                code: Mac::FUNCTION,
                held,
            } => !held.talk,
            Self::Key { held, .. } | Self::FlagsChanged { held, .. } => held.talk || keyboard_fn(),
        }
    }
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
    const LEFT_COMMAND: i64 = 55;
    const LEFT_CONTROL: i64 = 59;
    const LEFT_OPTION: i64 = 58;
    const LEFT_SHIFT: i64 = 56;
    const RIGHT_COMMAND: i64 = 54;
    const RIGHT_CONTROL: i64 = 62;
    const RIGHT_OPTION: i64 = 61;
    const RIGHT_SHIFT: i64 = 60;
    const SCREEN: i64 = 1;
    /// Command, Option, and Control, which make Fn part of another shortcut.
    const SHORTCUT_MODIFIERS: [i64; 6] = [
        Self::LEFT_COMMAND,
        Self::RIGHT_COMMAND,
        Self::LEFT_OPTION,
        Self::RIGHT_OPTION,
        Self::LEFT_CONTROL,
        Self::RIGHT_CONTROL,
    ];
    const SPACE: i64 = 49;
    const WINDOW: i64 = 13;

    /// Ends dictation for a session change, then waits for Fn's release.
    pub(super) fn interrupt(&mut self, input: &InputSender) {
        self.chord.interrupt_dictation(input);
    }

    /// Key events went by unseen while the tap was off, so no Shift tap pairs across the gap, and a
    /// hold, whose edges the owner can no longer trust, ends and waits for Fn's release. A dictation
    /// already released needs no more key edges, so it keeps its paste.
    pub(super) fn missed_events(&mut self, input: &InputSender) {
        self.taps.interrupt();
        if self.chord.interrupt() {
            Action::Interrupt.deliver(input);
        }
    }

    /// Decides one event-tap event at `at`, first ending a hold whose Fn release the tap missed, as
    /// the event shows. `keyboard_fn` reports whether the keyboard itself holds Fn; it is asked only
    /// during a hold, to confirm event flags that leave Fn out.
    pub(super) fn observe(
        &mut self,
        event: MacEvent,
        keyboard_fn: impl FnOnce() -> bool,
        at: Instant,
    ) -> Decision {
        let missed_release =
            self.chord.awaits_release() && self.chord.reconcile(event.fn_held_before(keyboard_fn));
        self.decide(event, at).after_missed_release(missed_release)
    }

    fn decide(&mut self, event: MacEvent, at: Instant) -> Decision {
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
                let pinned = self.taps.shift(held.shift, held.other_shortcut, at);
                Decision::pass(pinned.map(Action::Pin))
            },
            // A change that leaves one of them held is a press.
            MacEvent::FlagsChanged { code, held } if Self::SHORTCUT_MODIFIERS.contains(&code) => {
                self.taps.interrupt();
                self.chord.other_key(held.other_shortcut)
            },
            MacEvent::FlagsChanged { .. } => {
                self.taps.interrupt();
                Decision::pass(None)
            },
            MacEvent::Key { code, down, .. } => {
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
