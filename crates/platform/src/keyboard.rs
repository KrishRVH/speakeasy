use std::{
    mem,
    time::{Duration, Instant},
};

use crate::{Input, InputSender, Scope, Taps};

const SHIFT_TAP: Duration = Duration::from_millis(300);

const SHIFT_TAP_GAP: Duration = Duration::from_millis(400);

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

    fn reconcile(&mut self, held: bool) -> bool {
        !held && self.release().is_some()
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Hold {
    #[default]
    Released,
    Held,
    Interrupted,
}

#[derive(Clone, Copy)]
enum HoldKey {
    Space,
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
    pub(super) other_shortcut: bool,
    pub(super) talk: bool,
}

impl Modifiers {
    pub(super) const fn any(self) -> bool {
        self.shift || self.other_shortcut || self.talk
    }

    pub(super) const fn draws(self) -> bool {
        self.shift && !self.other_shortcut
    }
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy)]
pub(super) struct Physical {
    chord_keys: [bool; 4],
    other_shortcut: bool,
}

#[cfg(any(target_os = "windows", test))]
impl Physical {
    pub(super) fn read(down: impl Fn(u16) -> bool) -> Self {
        Self {
            chord_keys: Windows::CHORD_KEYS.map(&down),
            other_shortcut: Windows::modifiers(down).other_shortcut,
        }
    }
}

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

    pub(super) fn modifiers(down: impl Fn(u16) -> bool) -> Modifiers {
        let control = down(Self::CONTROL);
        let windows = down(Self::LEFT_WINDOWS) || down(Self::RIGHT_WINDOWS);
        Modifiers {
            shift: down(Self::SHIFT),
            other_shortcut: down(Self::ALT) || control != windows,
            talk: control && windows,
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
        let missed_release = self.reconcile(physical.chord_keys);
        self.decide(key, down, physical, at)
            .after_missed_release(missed_release)
    }

    fn reconcile(&mut self, chord_keys: [bool; 4]) -> bool {
        for (tracked, physically_down) in self.chord_keys_down.iter_mut().zip(chord_keys) {
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
            Self::ESCAPE => Decision::pass(down.then_some(Action::Escape)),
            Self::SPACE => self.chord.hold_key(HoldKey::Space, down),
            Self::SCREEN => self.chord.hold_key(HoldKey::Show(Scope::Screen), down),
            Self::WINDOW => self.chord.hold_key(HoldKey::Show(Scope::Window), down),
            Self::AREA => self.chord.hold_key(HoldKey::Show(Scope::Area), down),
            _ => self.chord.other_key(down),
        }
    }

    fn chord_key(&mut self, key: u16, down: bool) -> Option<Decision> {
        let tracked = Self::CHORD_KEYS
            .into_iter()
            .zip(&mut self.chord_keys_down)
            .find_map(|(chord_key, tracked)| (chord_key == key).then_some(tracked))?;
        let was_down = mem::replace(tracked, down);
        if was_down != down {
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

#[cfg(any(target_os = "macos", test))]
impl crate::MacShortcut {
    pub(super) const fn key_code(self) -> i64 {
        match self {
            Self::Function => Mac::FUNCTION,
            Self::RightOption => Mac::RIGHT_OPTION,
            Self::RightCommand => Mac::RIGHT_COMMAND,
        }
    }

    pub(super) const fn modifiers(self, flags: u64) -> Modifiers {
        const SHIFT: u64 = 0x0002_0000;
        const CONTROL: u64 = 0x0004_0000;
        const OPTION: u64 = 0x0008_0000;
        const COMMAND: u64 = 0x0010_0000;
        const FUNCTION: u64 = 0x0080_0000;
        let (talk_mask, own_group, opposite) = match self {
            Self::Function => (FUNCTION, FUNCTION, 0),
            Self::RightOption => (0x40, OPTION, 0x20),
            Self::RightCommand => (0x10, COMMAND, 0x08),
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
pub(super) enum MacEvent {
    Key {
        code: i64,
        down: bool,
        held: Modifiers,
    },
    FlagsChanged {
        code: i64,
        held: Modifiers,
    },
}

#[cfg(any(target_os = "macos", test))]
impl MacEvent {
    fn held_before(
        &self,
        shortcut: crate::MacShortcut,
        keyboard_held: impl FnOnce() -> bool,
    ) -> bool {
        match *self {
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

    pub(super) fn missed_events(&mut self, input: &InputSender) {
        self.taps.interrupt();
        if self.chord.interrupt() {
            Action::Interrupt.deliver(input);
        }
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

    fn decide(&mut self, event: MacEvent, at: Instant) -> Decision {
        match event {
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
            MacEvent::Key { code, down, .. } => {
                self.taps.interrupt();
                match code {
                    Self::ESCAPE => Decision::pass(down.then_some(Action::Escape)),
                    Self::SPACE | Self::SCREEN | Self::WINDOW | Self::AREA
                        if down
                            && self.shortcut != crate::MacShortcut::Function
                            && self.pressed.is_none_or(|pressed| {
                                at.saturating_duration_since(pressed) < Duration::from_millis(250)
                            }) =>
                    {
                        self.chord.other_key(true)
                    },
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
