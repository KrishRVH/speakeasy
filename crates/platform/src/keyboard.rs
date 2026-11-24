use std::{
    mem,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};

use crate::{
    Input, InputSender, Recorded, Scope, Taps,
    shortcut::{Chord, Modifier, ModifierSet, NativeShortcut, Pressed, Shortcuts, Side, Toggle},
};

const SHIFT_TAP: Duration = Duration::from_millis(300);

const SHIFT_TAP_GAP: Duration = Duration::from_millis(400);

/// A hold key this soon after a lone-modifier shortcut's press belongs to the app.
///
/// A deliberate hold tells gestures from ordinary shortcuts such as Ctrl+V or Command+S.
const APPLICATION_SHORTCUT: Duration = Duration::from_millis(250);

/// The talk shortcut's hold, and the hold keys it swallows.
#[derive(Default)]
struct Talk {
    hold: Hold,
    swallowing: u8,
}

impl Talk {
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
pub(super) enum Stroke {
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
    /// Another key ended the hold; nothing restarts it until the shortcut is released.
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
    Toggle(Toggle),
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
            Self::Toggle(Toggle::HandsFree) => input.deliver(Input::Toggle),
            Self::Toggle(Toggle::Show(scope)) => input.deliver(Input::ToggleShow(scope)),
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

    #[cfg(test)]
    pub(super) fn starts(&self) -> bool {
        self.action == Some(Action::Press)
    }

    /// Whether this decision acts on held modifiers, so Windows must not read a lone Win or Alt
    /// release afterwards as opening Start or a menu bar.
    #[cfg(any(target_os = "windows", test))]
    pub(super) const fn uses_modifiers(&self) -> bool {
        matches!(self.action, Some(Action::Press | Action::Toggle(_)))
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
    /// Every modifier of the talk shortcut is held; for a shortcut with a key, its key may not be.
    pub(super) talk: bool,
    /// Every modifier held, without sides, which a toggle shortcut must match exactly.
    pub(super) set: ModifierSet,
}

impl Modifiers {
    /// What `pressed` means for the talk shortcut `chord`.
    pub(super) fn of(chord: Chord, pressed: Pressed) -> Self {
        let talk = !chord.modifiers().is_empty() && chord.modifiers_held(pressed);
        Self {
            shift: pressed.has(Modifier::Shift, None),
            other_shortcut: chord.outside(pressed) || chord.partly_held(pressed),
            talk,
            set: pressed.modifiers(),
        }
    }

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
    pressed: Pressed,
    held: Modifiers,
    /// Refuses a talk press: a modifier outside the shortcut is held.
    ///
    /// Async key state lags the hook, so while a shortcut's second key goes down the first reads
    /// as part of it, which `held.other_shortcut` counts and this does not.
    chord_blocked: bool,
}

#[cfg(any(target_os = "windows", test))]
impl Physical {
    pub(super) fn read(shortcut: crate::WindowsShortcut, down: impl Fn(u16) -> bool) -> Self {
        let chord = shortcut.chord();
        let pressed = Windows::pressed(down);
        Self {
            pressed,
            held: Modifiers::of(chord, pressed),
            chord_blocked: chord.outside(pressed),
        }
    }
}

#[cfg(any(target_os = "windows", test))]
impl crate::WindowsShortcut {
    /// What the keys `down` reports held mean for this talk shortcut.
    ///
    /// A swallowed key, such as F13 alone, never reaches async key state, so it cannot read as
    /// held.
    pub(super) fn modifiers(self, down: impl Fn(u16) -> bool) -> Modifiers {
        Modifiers::of(self.chord(), Windows::pressed(down))
    }
}

/// A toggle shortcut's key, swallowed from the press that acted through its release.
#[derive(Default)]
struct ToggleKey(Option<u16>);

impl ToggleKey {
    /// Acts on a fresh press of a toggle's key with exactly its modifiers `held`.
    fn observe(
        &mut self,
        toggles: impl IntoIterator<Item = (Toggle, Chord)>,
        key: u16,
        stroke: Stroke,
        held: ModifierSet,
    ) -> Option<Decision> {
        match stroke {
            Stroke::Press => {
                let toggle = toggles.into_iter().find_map(|(toggle, chord)| {
                    (chord.code() == Some(key) && chord.exactly(held)).then_some(toggle)
                })?;
                self.0 = Some(key);
                Some(Decision::swallow(Some(Action::Toggle(toggle))))
            },
            Stroke::Repeat => (self.0 == Some(key)).then(|| Decision::swallow(None)),
            Stroke::Release => (self.0 == Some(key)).then(|| {
                self.0 = None;
                Decision::swallow(None)
            }),
        }
    }
}

/// The talk shortcut's own key, which only a press that completes the shortcut claims.
///
/// A claimed key is swallowed through its release. Alone, without modifiers, the key is
/// reserved, as F13 is, so its every stroke is swallowed.
fn talk_key(
    talk: &mut Talk,
    claimed: &mut bool,
    stroke: Stroke,
    completes: bool,
    reserved: bool,
) -> Option<Decision> {
    match stroke {
        Stroke::Press if completes => {
            *claimed = true;
            Some(Decision::swallow(talk.press()))
        },
        Stroke::Press | Stroke::Repeat => (*claimed || reserved).then(|| Decision::swallow(None)),
        Stroke::Release => {
            let claimed = mem::take(claimed);
            (claimed || reserved).then(|| Decision::swallow(talk.release()))
        },
    }
}

/// A hold key this soon after a single-modifier shortcut's press belongs to the app, as in
/// Right Ctrl+C; Fn has no app shortcuts to leave alone.
fn defers_to_applications(chord: Chord, pressed: Option<Instant>, at: Instant) -> bool {
    chord.code().is_none()
        && chord
            .modifiers()
            .single()
            .is_some_and(|modifier| modifier != Modifier::Function)
        && pressed
            .is_none_or(|pressed| at.saturating_duration_since(pressed) < APPLICATION_SHORTCUT)
}

fn toggle_chords<S: Copy>(
    shortcuts: &Shortcuts<S>,
    chord: impl Fn(S) -> Chord,
) -> impl Iterator<Item = (Toggle, Chord)> {
    shortcuts
        .toggles
        .iter()
        .map(move |(toggle, shortcut)| (toggle, chord(shortcut)))
}

#[cfg(any(target_os = "windows", test))]
#[derive(Default)]
pub(super) struct Windows {
    shortcuts: Shortcuts<crate::WindowsShortcut>,
    pressed: Option<Instant>,
    talk: Talk,
    /// Modifier keys seen down, corrected by async key state.
    modifiers: Pressed,
    /// The talk shortcut's key, claimed by a press that completed the shortcut.
    key_claimed: bool,
    toggle_key: ToggleKey,
    taps: ShiftTaps,
    keys: HeldKeys,
}

/// Keys seen down. The hook does not mark autorepeat, so another down of a held key is a repeat.
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
    const CONTROL: u16 = 0x11;
    const ESCAPE: u16 = 0x1B;
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
    const MODIFIER_KEYS: [(u16, Modifier, Side); 8] = [
        (Self::LEFT_CONTROL, Modifier::Control, Side::Left),
        (Self::RIGHT_CONTROL, Modifier::Control, Side::Right),
        (Self::LEFT_ALT, Modifier::Alt, Side::Left),
        (Self::RIGHT_ALT, Modifier::Alt, Side::Right),
        (Self::LEFT_SHIFT, Modifier::Shift, Side::Left),
        (Self::RIGHT_SHIFT, Modifier::Shift, Side::Right),
        (Self::LEFT_WINDOWS, Modifier::Super, Side::Left),
        (Self::RIGHT_WINDOWS, Modifier::Super, Side::Right),
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
    /// Side-less codes, which synthetic input can carry; async key state sets them for either
    /// side.
    const SIDELESS_KEYS: [(u16, Modifier); 3] = [
        (Self::CONTROL, Modifier::Control),
        (Self::ALT, Modifier::Alt),
        (Self::SHIFT, Modifier::Shift),
    ];
    const SPACE: u16 = 0x20;
    const WINDOW: u16 = 0x57;

    pub(super) fn new(shortcuts: Shortcuts<crate::WindowsShortcut>) -> Self {
        Self {
            shortcuts,
            ..Self::default()
        }
    }

    pub(super) fn interrupt(&mut self, input: &InputSender) {
        self.talk.interrupt_recording(input);
    }

    /// The modifier a key is, with its side; a side-less code reads as the left key.
    pub(super) fn modifier_key(key: u16) -> Option<(Modifier, Side)> {
        Self::MODIFIER_KEYS
            .into_iter()
            .find_map(|(code, modifier, side)| (code == key).then_some((modifier, side)))
            .or_else(|| {
                Self::SIDELESS_KEYS
                    .into_iter()
                    .find_map(|(code, modifier)| (code == key).then_some((modifier, Side::Left)))
            })
    }

    /// Modifier keys `down` reports held.
    ///
    /// A side-less code counts as the left key only when neither side reads as held.
    pub(super) fn pressed(down: impl Fn(u16) -> bool) -> Pressed {
        let sided = Self::MODIFIER_KEYS
            .into_iter()
            .filter(|(code, ..)| down(*code))
            .fold(Pressed::NONE, |pressed, (_, modifier, side)| {
                pressed.with(modifier, side, true)
            });
        Self::SIDELESS_KEYS
            .into_iter()
            .filter(|(code, modifier)| down(*code) && !sided.has(*modifier, None))
            .fold(sided, |pressed, (_, modifier)| {
                pressed.with(modifier, Side::Left, true)
            })
    }

    pub(super) fn observe(
        &mut self,
        key: u32,
        down: bool,
        physical: Physical,
        at: Instant,
    ) -> Decision {
        let key = u16::try_from(key).unwrap_or(u16::MAX);
        let missed_release = self.reconcile(physical.pressed);
        let stroke = self.keys.stroke(key, down);
        self.decide(key, stroke, physical, at)
            .after_missed_release(missed_release)
    }

    pub(super) fn types(key: u32, down: bool, decision: &Decision) -> bool {
        down && !decision.swallows()
            && u16::try_from(key).is_ok_and(|key| !Self::MODIFIERS.contains(&key))
    }

    /// Forgets modifier keys Windows reports up before this event and ends a hold they broke.
    ///
    /// The talk shortcut's own key is exempt: a claimed key never reaches async key state, so only
    /// its hook events establish and end its hold.
    fn reconcile(&mut self, pressed: Pressed) -> bool {
        self.modifiers = self.modifiers.within(pressed);
        self.talk.reconcile(self.talk_held())
    }

    fn talk_held(&self) -> bool {
        let chord = self.shortcuts.talk.chord();
        chord.modifiers_held(self.modifiers) && (chord.code().is_none() || self.key_claimed)
    }

    fn decide(&mut self, key: u16, stroke: Stroke, physical: Physical, at: Instant) -> Decision {
        let down = stroke != Stroke::Release;
        let chord = self.shortcuts.talk.chord();
        if let Some((modifier, side)) = Self::modifier_key(key) {
            let was_down = self.modifiers.has(modifier, Some(side));
            self.modifiers = self.modifiers.with(modifier, side, down);
            if chord.uses(modifier, side) {
                return self.talk_modifier(was_down, down, physical.chord_blocked, at);
            }
            // Shift never interrupts a hold, since a Show draws with it.
            if modifier == Modifier::Shift {
                let pinned = self.taps.shift(down, physical.held.other_shortcut, at);
                return Decision::pass(pinned.map(Action::Pin));
            }
            self.taps.interrupt();
            return self.talk.other_key(down);
        }
        if chord.code() == Some(key) {
            if stroke != Stroke::Repeat {
                self.taps.interrupt();
            }
            let completes = chord.modifiers_held(self.modifiers) && !physical.chord_blocked;
            if completes && stroke == Stroke::Press {
                self.pressed = Some(at);
            }
            let reserved = chord.modifiers().is_empty();
            if let Some(decision) = talk_key(
                &mut self.talk,
                &mut self.key_claimed,
                stroke,
                completes,
                reserved,
            ) {
                return decision;
            }
        }
        self.taps.interrupt();
        let toggles = toggle_chords(&self.shortcuts, crate::WindowsShortcut::chord);
        if let Some(decision) = self
            .toggle_key
            .observe(toggles, key, stroke, physical.held.set)
        {
            return decision;
        }
        if key == Self::ESCAPE {
            return Decision::pass(down.then_some(Action::Escape));
        }
        let Some(hold_key) = Self::hold_key(key) else {
            return self.talk.other_key(down);
        };
        if stroke == Stroke::Press && defers_to_applications(chord, self.pressed, at) {
            return self.talk.application_shortcut(hold_key);
        }
        self.talk.hold_key(hold_key, stroke)
    }

    /// A modifier of the talk shortcut went down or up.
    ///
    /// A shortcut with a key starts only from its key, so its modifiers can only end a hold.
    fn talk_modifier(
        &mut self,
        was_down: bool,
        down: bool,
        blocked: bool,
        at: Instant,
    ) -> Decision {
        // A shortcut key changing between taps makes a shortcut like Ctrl+Shift; one held through
        // both taps does not.
        if was_down != down {
            self.taps.interrupt();
        }
        let pressed = down && !was_down;
        if pressed {
            self.pressed = Some(at);
        }
        let keyed = self.shortcuts.talk.chord().code().is_some();
        let action = match (self.talk_held(), pressed) {
            (false, _) => self.talk.release(),
            (true, true) if blocked || keyed => None,
            (true, true) => self.talk.press(),
            (true, false) => None,
        };
        Decision::pass(action)
    }

    /// What a hook event means to a recording; `held` reports keys down before it.
    pub(super) fn recorded(key: u32, down: bool, held: impl Fn(u16) -> bool) -> RecordedKey {
        let Ok(key) = u16::try_from(key) else {
            return RecordedKey::Ignored;
        };
        if let Some((modifier, side)) = Self::modifier_key(key) {
            return RecordedKey::Modifiers(Self::pressed(held).with(modifier, side, down));
        }
        RecordedKey::Key {
            code: key,
            escape: key == Self::ESCAPE,
            stroke: Stroke::new(down, held(key)),
            implied: None,
        }
    }

    /// Notes a key event a recording took, so the next press of a key released meanwhile is not
    /// read as a repeat.
    pub(super) fn track(&mut self, key: u32, down: bool) {
        let key = u16::try_from(key).unwrap_or(u16::MAX);
        self.keys.stroke(key, down);
    }

    /// Whether Win or Alt is held once this event lands. Released with no other key, Win opens
    /// Start and Alt a menu bar, so a shortcut that used one must mark it used.
    pub(super) fn menu_held(key: u32, down: bool, held: impl Fn(u16) -> bool) -> bool {
        let pressed = match u16::try_from(key).ok().and_then(Self::modifier_key) {
            Some((modifier, side)) => Self::pressed(held).with(modifier, side, down),
            None => Self::pressed(held),
        };
        pressed.has(Modifier::Super, None) || pressed.has(Modifier::Alt, None)
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
}

#[cfg(any(target_os = "macos", test))]
impl crate::MacShortcut {
    /// What an event's flags mean for this talk shortcut.
    pub(super) fn modifiers(self, flags: u64) -> Modifiers {
        let chord = self.chord();
        let pressed = Mac::pressed(flags);
        // macOS sets the Fn flag on function and arrow keys, so it is not another modifier for a
        // shortcut with a key and no Fn. Toggles get every flag: each drops an Fn it lacks itself.
        let talk = if chord.code().is_some() && !chord.modifiers().contains(Modifier::Function) {
            pressed.without(Modifier::Function)
        } else {
            pressed
        };
        Modifiers {
            set: pressed.modifiers(),
            ..Modifiers::of(chord, talk)
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
    /// A flag change of the shortcut's own modifier reports its new state. Other events can carry
    /// synthetic flags, so an apparent release is confirmed against physical key state.
    fn held_before(&self, chord: Chord, keyboard_held: impl FnOnce() -> bool) -> bool {
        match *self {
            Self::Key {
                code, down, repeat, ..
            } if Mac::is_key(chord, code) => !down || repeat,
            Self::FlagsChanged { code, held }
                if Mac::modifier_key(code)
                    .is_some_and(|(modifier, side)| chord.uses(modifier, side)) =>
            {
                !held.talk
            },
            Self::Key { held, .. } | Self::FlagsChanged { held, .. } => {
                held.talk || keyboard_held()
            },
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
pub(super) struct Mac {
    shortcuts: Shortcuts<crate::MacShortcut>,
    pressed: Option<Instant>,
    talk: Talk,
    key_claimed: bool,
    toggle_key: ToggleKey,
    taps: ShiftTaps,
}

#[cfg(any(target_os = "macos", test))]
impl Mac {
    const AREA: i64 = 0;
    const ESCAPE: i64 = 53;
    /// Some keyboards, and remappers such as Karabiner-Elements, send the Globe key as this key
    /// beside Fn's flag change; it is Fn itself, never another key or typing.
    const GLOBE: i64 = 179;
    const KEYPAD_ENTER: i64 = 76;
    const MODIFIER_KEYS: [(i64, Modifier, Side); 9] = [
        (59, Modifier::Control, Side::Left),
        (62, Modifier::Control, Side::Right),
        (58, Modifier::Alt, Side::Left),
        (61, Modifier::Alt, Side::Right),
        (56, Modifier::Shift, Side::Left),
        (60, Modifier::Shift, Side::Right),
        (55, Modifier::Super, Side::Left),
        (54, Modifier::Super, Side::Right),
        (63, Modifier::Function, Side::Left),
    ];
    const PASTE_LAST: i64 = 9;
    const RETURN: i64 = 36;
    const SCREEN: i64 = 1;
    const SPACE: i64 = 49;
    const WINDOW: i64 = 13;

    pub(super) fn new(shortcuts: Shortcuts<crate::MacShortcut>) -> Self {
        Self {
            shortcuts,
            ..Self::default()
        }
    }

    pub(super) fn interrupt(&mut self, input: &InputSender) {
        self.talk.interrupt_recording(input);
    }

    pub(super) fn modifier_key(code: i64) -> Option<(Modifier, Side)> {
        Self::MODIFIER_KEYS
            .into_iter()
            .find_map(|(key, modifier, side)| (key == code).then_some((modifier, side)))
    }

    /// Modifier keys an event's flags report held.
    ///
    /// The low bits are side-specific device flags beside the aggregate `CGEvent` masks. A
    /// modifier whose aggregate flag is set without either side reads as its left key.
    pub(super) fn pressed(flags: u64) -> Pressed {
        const SIDES: [(u64, u64, u64, Modifier); 4] = [
            (0x0004_0000, 0x01, 0x2000, Modifier::Control),
            (0x0002_0000, 0x02, 0x04, Modifier::Shift),
            (0x0010_0000, 0x08, 0x10, Modifier::Super),
            (0x0008_0000, 0x20, 0x40, Modifier::Alt),
        ];
        const FUNCTION: u64 = 0x0080_0000;
        SIDES
            .into_iter()
            .fold(
                Pressed::NONE,
                |pressed, (aggregate, left, right, modifier)| {
                    let right_down = flags & right != 0;
                    let left_down = flags & left != 0 || (flags & aggregate != 0 && !right_down);
                    pressed.with(modifier, Side::Left, left_down).with(
                        modifier,
                        Side::Right,
                        right_down,
                    )
                },
            )
            .with(Modifier::Function, Side::Left, flags & FUNCTION != 0)
    }

    fn is_key(chord: Chord, code: i64) -> bool {
        chord.code().is_some_and(|key| i64::from(key) == code)
    }

    /// Whether every key of a talk shortcut reads as down by `key_down`, which reports HID key
    /// state; it confirms a hold when an event's flags may be synthetic.
    pub(super) fn chord_down(chord: Chord, key_down: impl Fn(u16) -> bool) -> bool {
        let modifier_down = |modifier| {
            Self::MODIFIER_KEYS.into_iter().any(|(code, key, side)| {
                key == modifier
                    && chord.side().is_none_or(|chosen| chosen == side)
                    && u16::try_from(code).is_ok_and(&key_down)
            })
        };
        chord.modifiers().iter().all(modifier_down) && chord.code().is_none_or(key_down)
    }

    /// Whether macOS sets the Fn flag on this key itself: function, arrow, and navigation keys.
    const fn sets_function(key: u16) -> bool {
        matches!(
            key,
            64 | 79 | 80 | 90 | 96..=101 | 103 | 105..=107 | 109 | 111 | 113..=126
        )
    }

    /// What a tap event means to a recording, with the flags Core Graphics reports on it.
    pub(super) fn recorded(event: MacEvent, flags: u64) -> RecordedKey {
        match event {
            MacEvent::FlagsChanged { .. } => RecordedKey::Modifiers(Self::pressed(flags)),
            MacEvent::Key {
                code: Self::GLOBE, ..
            } => RecordedKey::Ignored,
            MacEvent::Key {
                code, down, repeat, ..
            } => u16::try_from(code).map_or(RecordedKey::Ignored, |key| RecordedKey::Key {
                code: key,
                escape: code == Self::ESCAPE,
                stroke: Stroke::new(down, repeat),
                implied: Self::sets_function(key).then_some(Modifier::Function),
            }),
        }
    }

    pub(super) fn observe(
        &mut self,
        event: MacEvent,
        keyboard_held: impl FnOnce() -> bool,
        at: Instant,
    ) -> Decision {
        let chord = self.shortcuts.talk.chord();
        let missed_release = self.talk.awaits_release()
            && self.talk.reconcile(event.held_before(chord, keyboard_held));
        self.decide(event, at).after_missed_release(missed_release)
    }

    pub(super) const fn types(event: MacEvent, decision: &Decision) -> bool {
        matches!(event, MacEvent::Key { down: true, code, .. } if code != Self::GLOBE)
            && !decision.swallows()
    }

    fn decide(&mut self, event: MacEvent, at: Instant) -> Decision {
        let chord = self.shortcuts.talk.chord();
        match event {
            MacEvent::Key {
                code,
                down,
                repeat,
                held,
            } if Self::is_key(chord, code) => {
                let stroke = Stroke::new(down, repeat);
                if stroke != Stroke::Repeat {
                    self.taps.interrupt();
                }
                let completes = (chord.modifiers().is_empty() || held.talk) && !held.other_shortcut;
                if completes && stroke == Stroke::Press {
                    self.pressed = Some(at);
                }
                let reserved = chord.modifiers().is_empty();
                talk_key(
                    &mut self.talk,
                    &mut self.key_claimed,
                    stroke,
                    completes,
                    reserved,
                )
                .unwrap_or_else(|| self.key(code, stroke, held, at))
            },
            MacEvent::FlagsChanged { code, held }
                if Self::modifier_key(code)
                    .is_some_and(|(modifier, side)| chord.uses(modifier, side)) =>
            {
                self.taps.interrupt();
                // A shortcut with a key starts only from its key, so its modifiers only end it.
                if chord.code().is_some() {
                    return Decision::pass(if held.talk { None } else { self.talk.release() });
                }
                self.pressed = held.talk.then_some(at);
                Decision::pass(if held.talk && held.other_shortcut {
                    None
                } else if held.talk {
                    self.talk.press()
                } else {
                    self.talk.release()
                })
            },
            MacEvent::FlagsChanged { code, held }
                if Self::modifier_key(code)
                    .is_some_and(|(modifier, _)| modifier == Modifier::Shift) =>
            {
                let pinned = self.taps.shift(held.shift, held.other_shortcut, at);
                Decision::pass(pinned.map(Action::Pin))
            },
            MacEvent::FlagsChanged { code, held } if Self::modifier_key(code).is_some() => {
                self.taps.interrupt();
                self.talk.other_key(held.other_shortcut)
            },
            MacEvent::FlagsChanged { .. } => {
                self.taps.interrupt();
                Decision::pass(None)
            },
            MacEvent::Key {
                code: Self::GLOBE, ..
            } if chord.modifiers().contains(Modifier::Function) => Decision::pass(None),
            MacEvent::Key {
                code,
                down,
                repeat,
                held,
            } => self.key(code, Stroke::new(down, repeat), held, at),
        }
    }

    /// A key outside the talk shortcut: a toggle's, a hold key, Esc, or another that interrupts.
    fn key(&mut self, code: i64, stroke: Stroke, held: Modifiers, at: Instant) -> Decision {
        self.taps.interrupt();
        let toggles = toggle_chords(&self.shortcuts, crate::MacShortcut::chord);
        if let Ok(key) = u16::try_from(code)
            && let Some(decision) = self.toggle_key.observe(toggles, key, stroke, held.set)
        {
            return decision;
        }
        let chord = self.shortcuts.talk.chord();
        match Self::hold_key(code) {
            Some(key)
                if stroke == Stroke::Press && defers_to_applications(chord, self.pressed, at) =>
            {
                self.talk.application_shortcut(key)
            },
            Some(key) => self.talk.hold_key(key, stroke),
            None if code == Self::ESCAPE => {
                Decision::pass((stroke != Stroke::Release).then_some(Action::Escape))
            },
            None => self.talk.other_key(stroke != Stroke::Release),
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
}

/// Records the next shortcut pressed while every shortcut waits.
///
/// Modifier keys pass through, so apps never see one stuck down; any other key is swallowed and
/// ends the recording, and Esc alone cancels it.
#[derive(Default)]
pub(super) struct Recorder {
    held: Pressed,
    /// Every modifier held since the recording began, for a shortcut of modifiers alone.
    peak: Pressed,
}

impl Recorder {
    /// Modifier keys now `held`; releasing every one records those held.
    pub(super) fn modifiers(&mut self, held: Pressed) -> Recorded {
        self.held = held;
        self.peak = self.peak.union(held);
        if held.is_empty() && !self.peak.is_empty() {
            return Self::chosen(self.peak, None);
        }
        Recorded::Holding(
            Chord::recorded(held.modifiers(), held.single(), None).map(NativeShortcut::new),
        )
    }

    /// A fresh press of a key that is not a modifier, which ends the recording.
    ///
    /// A modifier the system sets on the key itself is left out, so F5 records as F5 whether or
    /// not the keyboard needed Fn to send it.
    pub(super) fn key(&self, code: u16, escape: bool, implied: Option<Modifier>) -> Recorded {
        if escape && self.held.is_empty() {
            return Recorded::Cancelled;
        }
        let held = implied.map_or(self.held, |modifier| self.held.without(modifier));
        Self::chosen(held, Some(code))
    }

    fn chosen(held: Pressed, key: Option<u16>) -> Recorded {
        let sided = key.is_none().then(|| held.single()).flatten();
        Chord::recorded(held.modifiers(), sided, key).map_or(Recorded::Cancelled, |chord| {
            Recorded::Chosen(NativeShortcut::new(chord))
        })
    }
}

/// A key event as a recording sees it.
#[derive(Clone, Copy)]
pub(super) enum RecordedKey {
    /// The modifier keys held once the event lands.
    Modifiers(Pressed),
    Key {
        code: u16,
        escape: bool,
        stroke: Stroke,
        /// A modifier the system sets on this key itself, as macOS sets Fn on F5.
        implied: Option<Modifier>,
    },
    /// Part of another key, as the Globe key is part of Fn.
    Ignored,
}

/// A shortcut recording Settings started, which the native key callback serves.
///
/// While one runs, keys go to it instead of the shortcuts.
#[derive(Default)]
pub(crate) struct Recording(
    #[expect(
        clippy::disallowed_types,
        reason = "Settings starts and stops a recording on the UI thread while the native key callback serves it on the input thread, which has no channel to receive it on"
    )]
    Mutex<Slot>,
);

#[derive(Default)]
enum Slot {
    #[default]
    Idle,
    Listening(Recorder, Sender<Recorded>),
    /// The key that ended a recording, withheld through its release as its press was, so the
    /// focused app sees neither its repeats nor a release without a press.
    Releasing(u16),
}

impl Recording {
    /// Starts a fresh recording, ending any other.
    ///
    /// The receiver reports the keys held, then the result, and closes when the recording ends.
    /// Dropping it ends the recording before the next key.
    pub(crate) fn start(&self) -> Receiver<Recorded> {
        let (sender, receiver) = async_channel::unbounded();
        *self.lock() = Slot::Listening(Recorder::default(), sender);
        receiver
    }

    /// Offers a key event to a running recording: whether to swallow it, or `None` when none runs
    /// and the shortcuts decide.
    ///
    /// Only a fresh press of a key that is not a modifier is swallowed, with that key's repeats and
    /// release after it. Other releases and repeats may belong to a key held from before the
    /// recording, which the app that saw it go down needs.
    pub(super) fn observe(&self, key: RecordedKey) -> Option<bool> {
        // Held through `*slot = next`, so a recording Settings starts meanwhile is never
        // overwritten.
        let mut slot = self.lock();
        let (next, swallow) = match (&mut *slot, key) {
            (Slot::Releasing(released), RecordedKey::Key { code, stroke, .. })
                if *released == code =>
            {
                match stroke {
                    Stroke::Repeat => return Some(true),
                    Stroke::Release => (Slot::Idle, Some(true)),
                    // Its release went missing, so the shortcuts own the key again.
                    Stroke::Press => (Slot::Idle, None),
                }
            },
            (Slot::Idle | Slot::Releasing(_), _) => return None,
            (Slot::Listening(_, events), _) if events.is_closed() => (Slot::Idle, None),
            (Slot::Listening(recorder, events), key) => {
                let (outcome, released) = match key {
                    RecordedKey::Modifiers(held) => (recorder.modifiers(held), None),
                    RecordedKey::Key {
                        code,
                        escape,
                        stroke: Stroke::Press,
                        implied,
                    } => (recorder.key(code, escape, implied), Some(code)),
                    RecordedKey::Key { .. } | RecordedKey::Ignored => return Some(false),
                };
                let last = !matches!(outcome, Recorded::Holding(_));
                // A closed receiver means Settings stopped listening.
                if events.try_send(outcome).is_ok() && !last {
                    return Some(false);
                }
                (
                    released.map_or(Slot::Idle, Slot::Releasing),
                    Some(released.is_some()),
                )
            },
        };
        *slot = next;
        swallow
    }

    /// Ignores poison: a recording holds no invariant a panic mid-update could break.
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
