use std::{
    mem,
    time::{Duration, Instant},
};

use crate::{Input, InputSender, Scope, Taps};

const SHIFT_TAP: Duration = Duration::from_millis(300);

const SHIFT_TAP_GAP: Duration = Duration::from_millis(400);

/// A hold key this soon after a right-hand modifier shortcut's press belongs to the app.
///
/// A deliberate hold tells gestures from ordinary shortcuts such as Ctrl+V or Command+S.
const APPLICATION_SHORTCUT: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Chord {
    hold: Hold,
    swallowing: u8,
}

impl Chord {
    fn press(&mut self) -> Option<Action> {
        if self.hold != Hold::Released {
            return None;
        }
        self.hold = Hold::Held;
        Some(Action::Press)
    }

    fn release(&mut self) -> Option<Action> {
        (mem::take(&mut self.hold) == Hold::Held).then_some(Action::Release)
    }

    /// Swallows a key pressed during a hold until its release, even after the hold ends.
    ///
    /// Only a fresh press during a live hold acts, so a repeat or a key held from before asks
    /// for nothing.
    fn hold_key(&mut self, key: HoldKey, stroke: Stroke) -> Decision {
        let bit = key.bit();
        let acts = stroke == Stroke::Press && self.hold == Hold::Held;
        if stroke == Stroke::Press {
            self.swallowing = if acts {
                self.swallowing | bit
            } else {
                self.swallowing & !bit
            };
        }
        let swallowed = self.swallowing & bit != 0;
        if stroke == Stroke::Release {
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

    fn application_shortcut(&mut self, key: HoldKey) -> Decision {
        self.swallowing &= !key.bit();
        self.other_key(true)
    }

    fn interrupt(&mut self) -> bool {
        let held = self.hold == Hold::Held;
        if held {
            self.hold = Hold::Interrupted;
        }
        held
    }

    fn interrupt_recording(&mut self, input: &InputSender) {
        Action::Interrupt.deliver(input);
        self.interrupt();
    }

    #[cfg(any(target_os = "macos", test))]
    fn awaits_release(&self) -> bool {
        self.hold != Hold::Released
    }

    /// Ends a hold whose chord was not `held` just before this event, its release unseen.
    ///
    /// That happens on the secure desktop or across sleep. Returns whether an uninterrupted hold
    /// ended, whose release the owner still has to hear.
    fn reconcile(&mut self, held: bool) -> bool {
        !held && self.release().is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stroke {
    Press,
    Repeat,
    Release,
}

impl Stroke {
    const fn new(down: bool, repeat: bool) -> Self {
        match (down, repeat) {
            (false, _) => Self::Release,
            (true, false) => Self::Press,
            (true, true) => Self::Repeat,
        }
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

#[derive(Clone, Copy)]
enum HoldKey {
    Space,
    PasteLast,
    Submit,
    Show(Scope),
}

impl HoldKey {
    const fn action(self) -> Action {
        match self {
            Self::Space => Action::Lock,
            Self::PasteLast => Action::PasteLast,
            Self::Submit => Action::Submit,
            Self::Show(scope) => Action::Show(scope),
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::Space => 1,
            Self::Show(Scope::Screen) => 2,
            Self::Show(Scope::Window) => 4,
            Self::Show(Scope::Area) => 8,
            Self::PasteLast => 16,
            Self::Submit => 32,
        }
    }
}

/// Two quick taps of Shift alone, which drop a pin during a Show.
///
/// Holding the talk shortcut through both taps does not break a pair. Every tap also starts the
/// next pair, and the Show picks which pairs pin, since only it sees whether a tap drew ink.
#[derive(Clone, Copy, Default)]
enum ShiftTaps {
    #[default]
    Idle,
    Down {
        at: Instant,
        first: Option<Instant>,
    },
    Tapped {
        pressed: Instant,
        released: Instant,
    },
}

impl ShiftTaps {
    fn shift(&mut self, down: bool, other_shortcut: bool, at: Instant) -> Option<Taps> {
        if other_shortcut {
            self.interrupt();
            return None;
        }
        let (next, doubled) = match (*self, down) {
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

    fn interrupt(&mut self) {
        *self = Self::Idle;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Press,
    Release,
    Lock,
    PasteLast,
    Submit,
    Show(Scope),
    Pin(Taps),
    Escape,
    Interrupt,
}

impl Action {
    fn deliver(self, input: &InputSender) {
        match self {
            Self::Press => input.deliver(Input::Press),
            Self::Release => input.deliver(Input::Release),
            Self::Lock => input.deliver(Input::Lock),
            Self::PasteLast => input.deliver(Input::PasteLast),
            Self::Submit => input.deliver(Input::Submit),
            Self::Show(scope) => input.deliver(Input::Show(scope)),
            Self::Pin(taps) => input.deliver(Input::Pin(taps)),
            Self::Escape => input.deliver(Input::Escape),
            Self::Interrupt => {
                input.deliver(Input::Interrupt);
                input.deliver(Input::Release);
            },
        }
    }
}

pub(super) struct Decision {
    /// The hold ended unseen before this event; the owner hears that release before `action`.
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

    fn actions(&self) -> impl Iterator<Item = Action> {
        self.missed_release
            .then_some(Action::Release)
            .into_iter()
            .chain(self.action)
    }

    pub(super) const fn swallows(&self) -> bool {
        self.swallowed
    }

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

#[derive(Clone, Copy)]
pub(super) struct Modifiers {
    pub(super) shift: bool,
    /// A modifier outside the talk shortcut, or only part of it: Shift then means a shortcut.
    pub(super) other_shortcut: bool,
    pub(super) talk: bool,
}

impl Modifiers {
    /// Insertion waits while any is held, so a paste never merges with the shortcut.
    pub(super) const fn any(self) -> bool {
        self.shift || self.other_shortcut || self.talk
    }

    /// Shift alone draws; the talk shortcut does not count, since it may still be held after S.
    pub(super) const fn draws(self) -> bool {
        self.shift && !self.other_shortcut
    }
}

/// Keys Windows reports physically held just before a hook event.
///
/// They correct for events the hook missed, such as releases on the secure desktop.
#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy)]
pub(super) struct Physical {
    chord_keys: [bool; 5],
    other_shortcut: bool,
    /// Refuses a chord press; Ctrl+Win checks only Alt.
    ///
    /// Async key state lags the hook, so while the chord's second key goes down the first reads
    /// as a lone Ctrl or Win, which `other_shortcut` counts.
    chord_blocked: bool,
}

#[cfg(any(target_os = "windows", test))]
impl Physical {
    pub(super) fn read(shortcut: crate::WindowsShortcut, down: impl Fn(u16) -> bool) -> Self {
        let other_shortcut = shortcut.modifiers(&down).other_shortcut;
        Self {
            chord_keys: Windows::CHORD_KEYS.map(&down),
            other_shortcut,
            chord_blocked: match shortcut {
                crate::WindowsShortcut::ControlWindows => down(Windows::ALT),
                _ => other_shortcut,
            },
        }
    }
}

#[cfg(any(target_os = "windows", test))]
impl crate::WindowsShortcut {
    pub(super) fn modifiers(self, down: impl Fn(u16) -> bool) -> Modifiers {
        let alt = down(Windows::ALT);
        let control = down(Windows::CONTROL);
        let windows = down(Windows::LEFT_WINDOWS) || down(Windows::RIGHT_WINDOWS);
        let (talk, other_shortcut) = match self {
            Self::ControlWindows => (control && windows, alt || control != windows),
            Self::RightControl => {
                let right = down(Windows::RIGHT_CONTROL);
                (right, alt || windows || down(Windows::LEFT_CONTROL))
            },
            // A swallowed F13 never reaches async key state, so it cannot read as held.
            Self::F13 => (false, alt || control || windows),
        };
        Modifiers {
            shift: down(Windows::SHIFT),
            other_shortcut,
            talk,
        }
    }

    const fn holds(self, key: u16) -> bool {
        match self {
            Self::ControlWindows => matches!(
                key,
                Windows::LEFT_CONTROL
                    | Windows::RIGHT_CONTROL
                    | Windows::LEFT_WINDOWS
                    | Windows::RIGHT_WINDOWS
            ),
            Self::RightControl => key == Windows::RIGHT_CONTROL,
            Self::F13 => key == Windows::F13,
        }
    }

    const fn held(self, keys: [bool; 5]) -> bool {
        let [
            left_control,
            right_control,
            left_windows,
            right_windows,
            f13,
        ] = keys;
        match self {
            Self::ControlWindows => {
                (left_control || right_control) && (left_windows || right_windows)
            },
            Self::RightControl => right_control,
            Self::F13 => f13,
        }
    }
}

#[cfg(any(target_os = "windows", test))]
#[derive(Default)]
pub(super) struct Windows {
    shortcut: crate::WindowsShortcut,
    pressed: Option<Instant>,
    chord: Chord,
    chord_keys_down: [bool; 5],
    taps: ShiftTaps,
    keys: HeldKeys,
}

/// Keys seen down. The hook reports no autorepeat, so another down of one is a repeat.
#[cfg(any(target_os = "windows", test))]
#[derive(Default)]
struct HeldKeys(Vec<u16>);

#[cfg(any(target_os = "windows", test))]
impl HeldKeys {
    fn stroke(&mut self, key: u16, down: bool) -> Stroke {
        let held = self.0.contains(&key);
        if !down {
            self.0.retain(|other| *other != key);
        } else if !held {
            self.0.push(key);
        }
        Stroke::new(down, held)
    }
}

#[cfg(any(target_os = "windows", test))]
impl Windows {
    const ALT: u16 = 0x12;
    const AREA: u16 = 0x41;
    const CAPS_LOCK: u16 = 0x14;
    const CHORD_KEYS: [u16; 5] = [
        Self::LEFT_CONTROL,
        Self::RIGHT_CONTROL,
        Self::LEFT_WINDOWS,
        Self::RIGHT_WINDOWS,
        Self::F13,
    ];
    const CONTROL: u16 = 0x11;
    const ESCAPE: u16 = 0x1B;
    const F13: u16 = 0x7C;
    const LEFT_ALT: u16 = 0xA4;
    const LEFT_CONTROL: u16 = 0xA2;
    const LEFT_SHIFT: u16 = 0xA0;
    const LEFT_WINDOWS: u16 = 0x5B;
    const MODIFIERS: [u16; 14] = [
        Self::SHIFT,
        Self::LEFT_SHIFT,
        Self::RIGHT_SHIFT,
        Self::CONTROL,
        Self::LEFT_CONTROL,
        Self::RIGHT_CONTROL,
        Self::ALT,
        Self::LEFT_ALT,
        Self::RIGHT_ALT,
        Self::LEFT_WINDOWS,
        Self::RIGHT_WINDOWS,
        Self::CAPS_LOCK,
        Self::NUM_LOCK,
        Self::SCROLL_LOCK,
    ];
    const NUM_LOCK: u16 = 0x90;
    const PASTE_LAST: u16 = 0x56;
    const RETURN: u16 = 0x0D;
    const RIGHT_ALT: u16 = 0xA5;
    const RIGHT_CONTROL: u16 = 0xA3;
    const RIGHT_SHIFT: u16 = 0xA1;
    const RIGHT_WINDOWS: u16 = 0x5C;
    const SCREEN: u16 = 0x53;
    const SCROLL_LOCK: u16 = 0x91;
    const SHIFT: u16 = 0x10;
    const SHIFT_KEYS: [u16; 3] = [Self::SHIFT, Self::LEFT_SHIFT, Self::RIGHT_SHIFT];
    const SPACE: u16 = 0x20;
    const WINDOW: u16 = 0x57;

    pub(super) fn new(shortcut: crate::WindowsShortcut) -> Self {
        Self {
            shortcut,
            ..Self::default()
        }
    }

    pub(super) fn interrupt(&mut self, input: &InputSender) {
        self.chord.interrupt_recording(input);
    }

    pub(super) fn observe(
        &mut self,
        key: u32,
        down: bool,
        physical: Physical,
        at: Instant,
    ) -> Decision {
        let key = u16::try_from(key).unwrap_or(u16::MAX);
        let missed_release = self.reconcile(physical.chord_keys);
        let stroke = self.keys.stroke(key, down);
        self.decide(key, stroke, physical, at)
            .after_missed_release(missed_release)
    }

    fn chord_key_down(&mut self, key: u16) -> Option<&mut bool> {
        Self::CHORD_KEYS
            .into_iter()
            .zip(&mut self.chord_keys_down)
            .find_map(|(chord_key, down)| (chord_key == key).then_some(down))
    }

    pub(super) fn types(key: u32, down: bool, decision: &Decision) -> bool {
        down && !decision.swallows()
            && u16::try_from(key).is_ok_and(|key| !Self::MODIFIERS.contains(&key))
    }

    /// Forgets chord keys Windows reports up before this event and ends a hold they broke.
    ///
    /// F13 is exempt: a swallowed F13 never reaches async key state, so only its own hook events
    /// establish and end its hold.
    fn reconcile(&mut self, chord_keys: [bool; 5]) -> bool {
        for ((key, tracked), physically_down) in Self::CHORD_KEYS
            .into_iter()
            .zip(self.chord_keys_down.iter_mut())
            .zip(chord_keys)
        {
            *tracked &= physically_down || key == Self::F13;
        }
        self.chord.reconcile(self.chord_held())
    }

    const fn chord_held(&self) -> bool {
        self.shortcut.held(self.chord_keys_down)
    }

    fn decide(&mut self, key: u16, stroke: Stroke, physical: Physical, at: Instant) -> Decision {
        let down = stroke != Stroke::Release;
        // Shift never interrupts a hold, since a Show draws with it.
        if Self::SHIFT_KEYS.contains(&key) {
            let pinned = self.taps.shift(down, physical.other_shortcut, at);
            return Decision::pass(pinned.map(Action::Pin));
        }
        if let Some(decision) = self.chord_key(key, down, physical.chord_blocked, at) {
            return decision;
        }
        self.taps.interrupt();
        if key == Self::ESCAPE {
            return Decision::pass(down.then_some(Action::Escape));
        }
        let Some(hold_key) = Self::hold_key(key) else {
            return self.chord.other_key(down);
        };
        if stroke == Stroke::Press && self.defers_to_applications(at) {
            return self.chord.application_shortcut(hold_key);
        }
        self.chord.hold_key(hold_key, stroke)
    }

    const fn hold_key(key: u16) -> Option<HoldKey> {
        match key {
            Self::SPACE => Some(HoldKey::Space),
            Self::PASTE_LAST => Some(HoldKey::PasteLast),
            Self::RETURN => Some(HoldKey::Submit),
            Self::SCREEN => Some(HoldKey::Show(Scope::Screen)),
            Self::WINDOW => Some(HoldKey::Show(Scope::Window)),
            Self::AREA => Some(HoldKey::Show(Scope::Area)),
            _ => None,
        }
    }

    fn chord_key(
        &mut self,
        key: u16,
        down: bool,
        other_shortcut: bool,
        at: Instant,
    ) -> Option<Decision> {
        if !self.shortcut.holds(key) {
            return None;
        }
        let was_down = mem::replace(self.chord_key_down(key)?, down);
        // A chord key changing between taps makes a shortcut like Ctrl+Shift; one held through
        // both taps does not.
        if was_down != down {
            self.taps.interrupt();
        }
        let pressed = down && !was_down;
        if pressed {
            self.pressed = Some(at);
        }
        let action = match (self.chord_held(), pressed) {
            (false, _) => self.chord.release(),
            (true, true) if other_shortcut => None,
            (true, true) => self.chord.press(),
            (true, false) => None,
        };
        Some(if self.shortcut == crate::WindowsShortcut::F13 {
            Decision::swallow(action)
        } else {
            Decision::pass(action)
        })
    }

    fn defers_to_applications(&self, at: Instant) -> bool {
        self.shortcut == crate::WindowsShortcut::RightControl
            && self
                .pressed
                .is_none_or(|pressed| at.saturating_duration_since(pressed) < APPLICATION_SHORTCUT)
    }
}

#[cfg(any(target_os = "macos", test))]
impl crate::MacShortcut {
    pub(super) const fn key_code(self) -> i64 {
        match self {
            Self::Function => Mac::FUNCTION,
            Self::RightOption => Mac::RIGHT_OPTION,
            Self::RightCommand => Mac::RIGHT_COMMAND,
            Self::F13 => Mac::F13,
        }
    }

    /// The low bits are side-specific device flags beside the aggregate `CGEvent` masks.
    ///
    /// Only the chosen right modifier is excluded; its left sibling must still interrupt a hold.
    pub(super) const fn modifiers(self, flags: u64) -> Modifiers {
        const LEFT_COMMAND: u64 = 0x08;
        const RIGHT_COMMAND: u64 = 0x10;
        const LEFT_OPTION: u64 = 0x20;
        const RIGHT_OPTION: u64 = 0x40;
        const SHIFT: u64 = 0x0002_0000;
        const CONTROL: u64 = 0x0004_0000;
        const OPTION: u64 = 0x0008_0000;
        const COMMAND: u64 = 0x0010_0000;
        const FUNCTION: u64 = 0x0080_0000;
        // Function-key events carry the Fn flag, which for F13 means no other shortcut.
        let flags = if matches!(self, Self::F13) {
            flags & !FUNCTION
        } else {
            flags
        };
        let (talk_mask, own_group, opposite) = match self {
            Self::Function => (FUNCTION, FUNCTION, 0),
            Self::RightOption => (RIGHT_OPTION, OPTION, LEFT_OPTION),
            Self::RightCommand => (RIGHT_COMMAND, COMMAND, LEFT_COMMAND),
            Self::F13 => (0, 0, 0),
        };
        let talk = flags & talk_mask != 0;
        let others = CONTROL | OPTION | COMMAND | FUNCTION;
        let other_shortcut = flags & (others & !own_group) != 0
            || flags & opposite != 0
            || (!talk && flags & own_group != 0);
        Modifiers {
            shift: flags & SHIFT != 0,
            talk,
            other_shortcut,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy)]
pub(super) enum MacEvent {
    Key {
        code: i64,
        down: bool,
        repeat: bool,
        held: Modifiers,
    },
    FlagsChanged {
        code: i64,
        held: Modifiers,
    },
}

#[cfg(any(target_os = "macos", test))]
impl MacEvent {
    /// Whether the shortcut was held before this event.
    ///
    /// The shortcut's own flag change reports its new state. Other events can carry synthetic
    /// flags, so an apparent release is confirmed against physical key state.
    fn held_before(
        &self,
        shortcut: crate::MacShortcut,
        keyboard_held: impl FnOnce() -> bool,
    ) -> bool {
        match *self {
            Self::Key {
                code, down, repeat, ..
            } if shortcut == crate::MacShortcut::F13 && code == shortcut.key_code() => {
                !down || repeat
            },
            Self::FlagsChanged { code, held } if code == shortcut.key_code() => !held.talk,
            Self::Key { held, .. } | Self::FlagsChanged { held, .. } => {
                held.talk || keyboard_held()
            },
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
pub(super) struct Mac {
    shortcut: crate::MacShortcut,
    pressed: Option<Instant>,
    chord: Chord,
    taps: ShiftTaps,
}

#[cfg(any(target_os = "macos", test))]
impl Mac {
    const AREA: i64 = 0;
    const ESCAPE: i64 = 53;
    const F13: i64 = 105;
    const FUNCTION: i64 = 63;
    const KEYPAD_ENTER: i64 = 76;
    const LEFT_COMMAND: i64 = 55;
    const LEFT_CONTROL: i64 = 59;
    const LEFT_OPTION: i64 = 58;
    const LEFT_SHIFT: i64 = 56;
    const PASTE_LAST: i64 = 9;
    const RETURN: i64 = 36;
    const RIGHT_COMMAND: i64 = 54;
    const RIGHT_CONTROL: i64 = 62;
    const RIGHT_OPTION: i64 = 61;
    const RIGHT_SHIFT: i64 = 60;
    const SCREEN: i64 = 1;
    const SHORTCUT_MODIFIERS: [i64; 7] = [
        Self::FUNCTION,
        Self::LEFT_COMMAND,
        Self::RIGHT_COMMAND,
        Self::LEFT_OPTION,
        Self::RIGHT_OPTION,
        Self::LEFT_CONTROL,
        Self::RIGHT_CONTROL,
    ];
    const SPACE: i64 = 49;
    const WINDOW: i64 = 13;

    pub(super) fn new(shortcut: crate::MacShortcut) -> Self {
        Self {
            shortcut,
            ..Self::default()
        }
    }

    pub(super) fn interrupt(&mut self, input: &InputSender) {
        self.chord.interrupt_recording(input);
    }

    pub(super) fn observe(
        &mut self,
        event: MacEvent,
        keyboard_held: impl FnOnce() -> bool,
        at: Instant,
    ) -> Decision {
        let missed_release = self.chord.awaits_release()
            && self
                .chord
                .reconcile(event.held_before(self.shortcut, keyboard_held));
        self.decide(event, at).after_missed_release(missed_release)
    }

    pub(super) const fn types(event: MacEvent, decision: &Decision) -> bool {
        matches!(event, MacEvent::Key { down: true, .. }) && !decision.swallows()
    }

    fn decide(&mut self, event: MacEvent, at: Instant) -> Decision {
        match event {
            MacEvent::Key {
                code,
                down,
                repeat,
                held,
            } if self.shortcut == crate::MacShortcut::F13 && code == self.shortcut.key_code() => {
                if !repeat {
                    self.taps.interrupt();
                }
                Decision::swallow(if !down {
                    self.chord.release()
                } else if repeat || held.other_shortcut {
                    None
                } else {
                    self.chord.press()
                })
            },
            MacEvent::FlagsChanged { code, held } if code == self.shortcut.key_code() => {
                self.taps.interrupt();
                self.pressed = held.talk.then_some(at);
                Decision::pass(if held.talk && held.other_shortcut {
                    None
                } else if held.talk {
                    self.chord.press()
                } else {
                    self.chord.release()
                })
            },
            MacEvent::FlagsChanged {
                code: Self::LEFT_SHIFT | Self::RIGHT_SHIFT,
                held,
            } => {
                let pinned = self.taps.shift(held.shift, held.other_shortcut, at);
                Decision::pass(pinned.map(Action::Pin))
            },
            MacEvent::FlagsChanged { code, held } if Self::SHORTCUT_MODIFIERS.contains(&code) => {
                self.taps.interrupt();
                self.chord.other_key(held.other_shortcut)
            },
            MacEvent::FlagsChanged { .. } => {
                self.taps.interrupt();
                Decision::pass(None)
            },
            MacEvent::Key {
                code, down, repeat, ..
            } => {
                self.taps.interrupt();
                let stroke = Stroke::new(down, repeat);
                match Self::hold_key(code) {
                    Some(key) if stroke == Stroke::Press && self.defers_to_applications(at) => {
                        self.chord.application_shortcut(key)
                    },
                    Some(key) => self.chord.hold_key(key, stroke),
                    None if code == Self::ESCAPE => Decision::pass(down.then_some(Action::Escape)),
                    None => self.chord.other_key(down),
                }
            },
        }
    }

    const fn hold_key(code: i64) -> Option<HoldKey> {
        match code {
            Self::SPACE => Some(HoldKey::Space),
            Self::PASTE_LAST => Some(HoldKey::PasteLast),
            Self::RETURN | Self::KEYPAD_ENTER => Some(HoldKey::Submit),
            Self::SCREEN => Some(HoldKey::Show(Scope::Screen)),
            Self::WINDOW => Some(HoldKey::Show(Scope::Window)),
            Self::AREA => Some(HoldKey::Show(Scope::Area)),
            _ => None,
        }
    }

    fn defers_to_applications(&self, at: Instant) -> bool {
        matches!(
            self.shortcut,
            crate::MacShortcut::RightOption | crate::MacShortcut::RightCommand
        ) && self
            .pressed
            .is_none_or(|pressed| at.saturating_duration_since(pressed) < APPLICATION_SHORTCUT)
    }
}

#[cfg(test)]
mod tests;
