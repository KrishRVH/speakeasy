//! Shortcuts a person chooses or records: modifiers held together, with at most one other key.
//!
//! Settings stores each as text, such as `right_option` or `control+shift+space`, so a
//! hand-edited file stays readable. Windows and macOS name the same modifiers differently, and their
//! key codes differ, so each has its own type over one shared `Chord`.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// A modifier, named by what it does; each keyboard spells it its own way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Modifier {
    Control,
    /// Option on a Mac.
    Alt,
    Shift,
    /// Command on a Mac, Win on Windows.
    Super,
    /// Fn (🌐) on a Mac; Windows never sees it.
    Function,
}

impl Modifier {
    const ALL: [Self; 5] = [
        Self::Control,
        Self::Alt,
        Self::Shift,
        Self::Super,
        Self::Function,
    ];

    const fn bit(self) -> u8 {
        match self {
            Self::Control => 1,
            Self::Alt => 2,
            Self::Shift => 4,
            Self::Super => 8,
            Self::Function => 16,
        }
    }
}

/// Modifiers held together, without their sides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct ModifierSet(u8);

impl ModifierSet {
    #[cfg(test)]
    pub(crate) const EMPTY: Self = Self(0);
    const SHIFT: Self = Self(Modifier::Shift.bit());

    pub(crate) const fn with(self, modifier: Modifier) -> Self {
        Self(self.0 | modifier.bit())
    }

    pub(crate) const fn contains(self, modifier: Modifier) -> bool {
        self.0 & modifier.bit() != 0
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every modifier here is also in `other`.
    pub(crate) const fn within(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }

    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) const fn without(self, modifier: Modifier) -> Self {
        Self(self.0 & !modifier.bit())
    }

    pub(crate) fn iter(self) -> impl Iterator<Item = Modifier> {
        Modifier::ALL
            .into_iter()
            .filter(move |modifier| self.contains(*modifier))
    }

    pub(crate) fn single(self) -> Option<Modifier> {
        let mut held = self.iter();
        held.next().filter(|_| held.next().is_none())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    Left,
    Right,
}

/// Keys held together: modifiers, with at most one other key.
///
/// A lone modifier keeps the side it was recorded on, so Right Option leaves Left Option to apps.
/// Modifiers held together, or with a key, match on either side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Chord {
    modifiers: ModifierSet,
    side: Option<Side>,
    key: Option<u16>,
}

impl Chord {
    const fn lone(modifier: Modifier, side: Option<Side>) -> Self {
        Self {
            modifiers: ModifierSet(modifier.bit()),
            side,
            key: None,
        }
    }

    const fn key(code: u16) -> Self {
        Self {
            modifiers: ModifierSet(0),
            side: None,
            key: Some(code),
        }
    }

    /// The chord a recording ends with; `None` when nothing was held.
    ///
    /// `sided` names the one modifier held, with its side, when it was alone.
    pub(crate) fn recorded(
        modifiers: ModifierSet,
        sided: Option<(Modifier, Side)>,
        key: Option<u16>,
    ) -> Option<Self> {
        if modifiers.is_empty() && key.is_none() {
            return None;
        }
        let side = sided
            .filter(|(modifier, _)| {
                key.is_none()
                    && *modifier != Modifier::Function
                    && modifiers.single() == Some(*modifier)
            })
            .map(|(_, side)| side);
        Some(Self {
            modifiers,
            side,
            key,
        })
    }

    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) const fn modifiers(self) -> ModifierSet {
        self.modifiers
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) const fn side(self) -> Option<Side> {
        self.side
    }

    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) const fn code(self) -> Option<u16> {
        self.key
    }

    fn name(self, layout: &Layout) -> String {
        if let (Some(side), Some(modifier)) = (self.side, self.modifiers.single()) {
            let side = match side {
                Side::Left => "left",
                Side::Right => "right",
            };
            return format!("{side}_{}", layout.modifier(modifier).name);
        }
        layout
            .order
            .iter()
            .filter(|modifier| self.modifiers.contains(**modifier))
            .map(|modifier| layout.modifier(*modifier).name.to_owned())
            .chain(self.key.map(|code| layout.key_name(code)))
            .collect::<Vec<_>>()
            .join("+")
    }

    fn label(self, layout: &Layout) -> String {
        if let (Some(side), Some(modifier)) = (self.side, self.modifiers.single()) {
            let side = match side {
                Side::Left => "Left",
                Side::Right => "Right",
            };
            return format!("{side} {}", layout.modifier(modifier).label);
        }
        layout
            .order
            .iter()
            .filter(|modifier| self.modifiers.contains(**modifier))
            .map(|modifier| layout.modifier(*modifier).label.to_owned())
            .chain(self.key.map(|code| layout.key_label(code)))
            .collect::<Vec<_>>()
            .join(" + ")
    }

    fn parse(text: &str, layout: &Layout) -> Result<Self, String> {
        if let Some(chord) = layout
            .aliases
            .iter()
            .find_map(|(name, chord)| (*name == text).then_some(*chord))
        {
            return Ok(chord);
        }
        let unknown = |part: &str| format!("\"{part}\" isn't a key Speakeasy knows");
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if let [part] = parts.as_slice()
            && let Some(sided) = layout.sided(part)
        {
            return Ok(sided);
        }
        let mut modifiers = ModifierSet::default();
        let mut key = None;
        for part in parts {
            if key.is_some() {
                return Err(format!(
                    "\"{text}\" has a key after its last key; a shortcut has one key at most"
                ));
            }
            match layout.modifier_named(part) {
                Some(modifier) => modifiers = modifiers.with(modifier),
                None => key = Some(layout.key_code(part).ok_or_else(|| unknown(part))?),
            }
        }
        Self::recorded(modifiers, None, key)
            .ok_or_else(|| "A shortcut needs at least one key".to_owned())
    }

    /// Why this chord cannot be a shortcut, if it cannot.
    fn problem(self, layout: &Layout) -> Option<Problem> {
        let kind = self.key.map(|code| layout.kind(code));
        match kind {
            Some(Kind::Reserved) => return Some(Problem::Reserved),
            Some(Kind::Character) if self.modifiers.within(layout.typing) => {
                return Some(Problem::Types);
            },
            Some(Kind::Typing) if self.modifiers.within(ModifierSet::SHIFT) => {
                return Some(Problem::Types);
            },
            _ => {},
        }
        // AltGr held alone reads as Ctrl and Alt, so modifiers alone count too.
        if let Some(alt_graph) = layout.alt_graph
            && matches!(kind, None | Some(Kind::Character))
            && alt_graph.within(self.modifiers)
            && self.modifiers.within(alt_graph.with(Modifier::Shift))
        {
            return Some(Problem::AltGraph);
        }
        (self.key.is_none() && self.modifiers.single() == Some(Modifier::Shift))
            .then_some(Problem::ShiftAlone)
    }
}

/// Modifier keys held, each with its side; Fn has none.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pressed(u16);

#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl Pressed {
    pub(crate) const NONE: Self = Self(0);

    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The keys held in both.
    #[cfg(any(target_os = "windows", test))]
    pub(crate) const fn within(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    const fn bit(modifier: Modifier, side: Side) -> u16 {
        let (left, right) = match modifier {
            Modifier::Control => (0, 1),
            Modifier::Alt => (2, 3),
            Modifier::Shift => (4, 5),
            Modifier::Super => (6, 7),
            // Fn has one key, so both sides name it.
            Modifier::Function => (8, 8),
        };
        match side {
            Side::Left => 1 << left,
            Side::Right => 1 << right,
        }
    }

    #[must_use]
    pub(crate) const fn with(self, modifier: Modifier, side: Side, down: bool) -> Self {
        let bit = Self::bit(modifier, side);
        if down {
            Self(self.0 | bit)
        } else {
            Self(self.0 & !bit)
        }
    }

    /// Releases `modifier` on both sides.
    #[must_use]
    pub(crate) const fn without(self, modifier: Modifier) -> Self {
        self.with(modifier, Side::Left, false)
            .with(modifier, Side::Right, false)
    }

    /// Whether `modifier` is held on `side`, or on either side for `None`.
    pub(crate) const fn has(self, modifier: Modifier, side: Option<Side>) -> bool {
        match side {
            Some(side) => self.0 & Self::bit(modifier, side) != 0,
            None => {
                self.0 & (Self::bit(modifier, Side::Left) | Self::bit(modifier, Side::Right)) != 0
            },
        }
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The modifiers held, without their sides.
    pub(crate) fn modifiers(self) -> ModifierSet {
        Modifier::ALL
            .into_iter()
            .filter(|modifier| self.has(*modifier, None))
            .fold(ModifierSet::default(), ModifierSet::with)
    }

    /// The one modifier held, with its side, when only one key is down.
    pub(crate) fn single(self) -> Option<(Modifier, Side)> {
        let modifier = self.modifiers().single()?;
        [Side::Left, Side::Right]
            .into_iter()
            .filter(|side| self.has(modifier, Some(*side)))
            .fold(None, |found, side| match found {
                None => Some(Some((modifier, side))),
                Some(_) => Some(None),
            })
            .flatten()
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl Chord {
    /// Whether `pressed` holds all the chord's modifiers, on its side if it has one.
    pub(crate) fn modifiers_held(self, pressed: Pressed) -> bool {
        self.modifiers
            .iter()
            .all(|modifier| pressed.has(modifier, self.side))
    }

    /// Whether `pressed` holds a modifier outside the chord, or the other side of its one modifier.
    ///
    /// Shift outside the chord does not count: it draws during a Show and shifts typing, so callers
    /// read it on its own.
    pub(crate) fn outside(self, pressed: Pressed) -> bool {
        let opposite = self
            .side
            .zip(self.modifiers.single())
            .is_some_and(|(side, modifier)| {
                let other = match side {
                    Side::Left => Side::Right,
                    Side::Right => Side::Left,
                };
                pressed.has(modifier, Some(other))
            });
        let others = pressed.modifiers().without(Modifier::Shift);
        opposite || !others.within(self.modifiers)
    }

    /// Whether `pressed` holds some of the chord's modifiers but not all.
    pub(crate) fn partly_held(self, pressed: Pressed) -> bool {
        !self.modifiers_held(pressed)
            && self
                .modifiers
                .iter()
                .any(|modifier| pressed.has(modifier, self.side))
    }

    /// Whether a modifier key, pressed on `side`, is one the chord holds.
    pub(crate) fn uses(self, modifier: Modifier, side: Side) -> bool {
        self.modifiers.contains(modifier) && self.side.is_none_or(|own| own == side)
    }

    /// Whether `held` is exactly the chord's modifiers, as a key press needs.
    ///
    /// macOS adds the Fn flag to function and arrow keys, so Fn counts only for a chord that has
    /// it.
    pub(crate) fn exactly(self, held: ModifierSet) -> bool {
        let held = if self.modifiers.contains(Modifier::Function) {
            held
        } else {
            held.without(Modifier::Function)
        };
        held == self.modifiers
    }
}

/// Why keys cannot be a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A key that types, pressed alone or with only modifiers that type with it.
    Types,
    /// Esc cancels, and Caps Lock and Num Lock change how keys type.
    Reserved,
    /// Shift alone types capitals and draws during a Show.
    ShiftAlone,
    /// Ctrl and Alt together, which many Windows layouts read as `AltGr` and type characters with.
    AltGraph,
}

/// How a key may appear in a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Types a character the keyboard layout chooses, so it needs a modifier that types none:
    /// never Shift, nor Option on a Mac.
    Character,
    /// Edits text or moves the caret, so it needs a modifier other than Shift.
    Typing,
    /// Types nothing, so it may be a shortcut alone, as F13 is.
    Alone,
    Reserved,
}

#[derive(Clone, Copy)]
struct Named {
    name: &'static str,
    label: &'static str,
}

struct Key {
    code: u16,
    name: &'static str,
    label: &'static str,
    kind: Kind,
}

const fn key(code: u16, name: &'static str, label: &'static str, kind: Kind) -> Key {
    Key {
        code,
        name,
        label,
        kind,
    }
}

/// One platform's names for modifiers and keys.
struct Layout {
    /// Control, Alt, Shift, Super, and Function, in `Modifier::ALL` order; Windows has no Fn.
    modifiers: [Option<Named>; 5],
    /// The order labels list modifiers in.
    order: &'static [Modifier],
    keys: &'static [Key],
    /// Values earlier versions saved that the current grammar spells differently.
    aliases: &'static [(&'static str, Chord)],
    /// Modifiers that still type with a character key.
    typing: ModifierSet,
    /// Modifiers that together act as `AltGr`, if the platform has it.
    alt_graph: Option<ModifierSet>,
    /// The character the current keyboard layout prints on a key, when the system can say.
    printed: fn(u16) -> Option<char>,
}

impl Layout {
    fn modifier(&self, modifier: Modifier) -> &Named {
        let index = Modifier::ALL
            .iter()
            .position(|other| *other == modifier)
            .unwrap_or_default();
        self.modifiers
            .get(index)
            .and_then(Option::as_ref)
            .unwrap_or(&Named {
                name: "fn",
                label: "Fn",
            })
    }

    fn modifier_named(&self, name: &str) -> Option<Modifier> {
        Modifier::ALL
            .into_iter()
            .zip(&self.modifiers)
            .find_map(|(modifier, named)| {
                named
                    .as_ref()
                    .is_some_and(|named| named.name == name)
                    .then_some(modifier)
            })
    }

    fn sided(&self, name: &str) -> Option<Chord> {
        let (side, rest) = if let Some(rest) = name.strip_prefix("left_") {
            (Side::Left, rest)
        } else {
            (Side::Right, name.strip_prefix("right_")?)
        };
        let modifier = self
            .modifier_named(rest)
            .filter(|modifier| *modifier != Modifier::Function)?;
        Some(Chord::lone(modifier, Some(side)))
    }

    fn find(&self, code: u16) -> Option<&Key> {
        self.keys.iter().find(|key| key.code == code)
    }

    fn key_name(&self, code: u16) -> String {
        self.find(code)
            .map_or_else(|| format!("key_{code}"), |key| key.name.to_owned())
    }

    /// A character key shows what the current layout prints on it, so the A key on AZERTY reads
    /// "A" though its code is Q's. Digits keep their table label, which every layout prints too.
    fn key_label(&self, code: u16) -> String {
        let Some(key) = self.find(code) else {
            return format!("Key {code}");
        };
        (key.kind == Kind::Character && !key.label.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| (self.printed)(code))
            .flatten()
            .map_or_else(
                || key.label.to_owned(),
                |printed| {
                    // Key caps print letters in capitals; ß has no one-letter capital, so stays.
                    let mut upper = printed.to_uppercase();
                    match (upper.next(), upper.next()) {
                        (Some(upper), None) => upper.to_string(),
                        _ => printed.to_string(),
                    }
                },
            )
    }

    fn key_code(&self, name: &str) -> Option<u16> {
        self.keys
            .iter()
            .find_map(|key| (key.name == name).then_some(key.code))
            .or_else(|| name.strip_prefix("key_")?.parse().ok())
    }

    /// Keys missing from the table, such as media keys, type nothing.
    fn kind(&self, code: u16) -> Kind {
        self.find(code).map_or(Kind::Alone, |key| key.kind)
    }
}

macro_rules! platform_shortcut {
    ($(#[$meta:meta])* $name:ident, $os:literal, $layout:expr, $default:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name(Chord);

        impl $name {
            /// The keys as Settings shows them, such as "Right Option" or "Ctrl + Shift + K".
            #[must_use]
            pub fn label(self) -> String {
                self.0.label(&$layout)
            }

            /// Why these keys cannot be a shortcut, if they cannot.
            #[must_use]
            pub fn problem(self) -> Option<Problem> {
                self.0.problem(&$layout)
            }

            /// Whether the shortcut has a key besides modifiers, which macOS hides from
            /// Speakeasy during Secure Keyboard Entry.
            #[must_use]
            pub const fn has_key(self) -> bool {
                self.0.key.is_some()
            }

            /// Whether pressing `other` also completes this one, which then starts first, so
            /// `other` could never reach its own action.
            ///
            /// Modifiers alone start once all are held; a lone modifier with a side leaves the
            /// other side free for `other`. A shortcut with a key claims that key whenever its
            /// modifiers are held with it.
            #[must_use]
            pub const fn starts_within(self, other: Self) -> bool {
                let modifiers = self.0.modifiers.within(other.0.modifiers);
                match (self.0.key, other.0.key) {
                    (None, _) => modifiers && self.0.side.is_none(),
                    (Some(key), Some(other_key)) => modifiers && key == other_key,
                    (Some(_), None) => false,
                }
            }

            /// Whether this is one modifier alone, such as Right Option, whose quick combinations
            /// with other keys stay with apps.
            #[must_use]
            pub fn is_lone_modifier(self) -> bool {
                self.0.key.is_none()
                    && self
                        .0
                        .modifiers
                        .single()
                        .is_some_and(|modifier| modifier != Modifier::Function)
            }

            #[cfg(any(target_os = $os, test))]
            pub(crate) const fn chord(self) -> Chord {
                self.0
            }

            #[cfg(any(target_os = $os, test))]
            pub(crate) const fn new(chord: Chord) -> Self {
                Self(chord)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::$default
            }
        }

        impl Shortcuts<$name> {
            /// Why `shortcut` cannot serve `binding` beside the other shortcuts, if it cannot.
            #[must_use]
            pub fn refusal(&self, binding: Binding, shortcut: $name) -> Option<Refusal> {
                if let Some(problem) = shortcut.problem() {
                    return Some(Refusal::Problem(problem));
                }
                if binding != Binding::Talk && !shortcut.has_key() {
                    return Some(Refusal::NeedsKey);
                }
                self.bindings()
                    .filter(|(other, _)| *other != binding)
                    .find_map(|(other, keys)| match other {
                        _ if keys == shortcut => Some(Refusal::Taken(other)),
                        Binding::Talk => keys.starts_within(shortcut).then_some(Refusal::BehindTalk),
                        Binding::Toggle(toggle) => (binding == Binding::Talk
                            && shortcut.starts_within(keys))
                        .then_some(Refusal::Covers(toggle)),
                    })
            }

            /// The first shortcut the others refuse, as a hand-edited settings file can hold.
            #[must_use]
            pub fn refused(&self) -> Option<(Binding, Refusal)> {
                self.bindings()
                    .find_map(|(binding, keys)| Some((binding, self.refusal(binding, keys)?)))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0.name(&$layout))
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(text: &str) -> Result<Self, String> {
                Chord::parse(text, &$layout).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                text.parse().map_err(de::Error::custom)
            }
        }
    };
}

platform_shortcut!(
    /// A shortcut on a Mac, by `CGKeyCode`.
    MacShortcut,
    "macos",
    MAC,
    FUNCTION
);

platform_shortcut!(
    /// A shortcut on Windows, by virtual-key code.
    WindowsShortcut,
    "windows",
    WINDOWS,
    CONTROL_WINDOWS
);

impl MacShortcut {
    pub const F13: Self = Self(Chord::key(105));
    pub const FUNCTION: Self = Self(Chord::lone(Modifier::Function, None));
    pub const RIGHT_COMMAND: Self = Self(Chord::lone(Modifier::Super, Some(Side::Right)));
    pub const RIGHT_OPTION: Self = Self(Chord::lone(Modifier::Alt, Some(Side::Right)));
}

impl WindowsShortcut {
    pub const CONTROL_WINDOWS: Self = Self(Chord {
        modifiers: ModifierSet(Modifier::Control.bit() | Modifier::Super.bit()),
        side: None,
        key: None,
    });
    pub const F13: Self = Self(Chord::key(0x7C));
    pub const RIGHT_CONTROL: Self = Self(Chord::lone(Modifier::Control, Some(Side::Right)));
}

/// Optional shortcuts that each start an action and, pressed again, finish it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Toggles<S> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hands_free: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_screen: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_window: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_area: Option<S>,
}

impl<S: Copy> Toggles<S> {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    /// Each set shortcut with what it toggles.
    pub fn iter(&self) -> impl Iterator<Item = (Toggle, S)> {
        [
            (Toggle::HandsFree, self.hands_free),
            (Toggle::Show(crate::Scope::Screen), self.show_screen),
            (Toggle::Show(crate::Scope::Window), self.show_window),
            (Toggle::Show(crate::Scope::Area), self.show_area),
        ]
        .into_iter()
        .filter_map(|(toggle, shortcut)| Some((toggle, shortcut?)))
    }

    #[must_use]
    pub const fn get(&self, toggle: Toggle) -> Option<S> {
        match toggle {
            Toggle::HandsFree => self.hands_free,
            Toggle::Show(crate::Scope::Screen) => self.show_screen,
            Toggle::Show(crate::Scope::Window) => self.show_window,
            Toggle::Show(crate::Scope::Area) => self.show_area,
        }
    }

    pub fn set(&mut self, toggle: Toggle, shortcut: Option<S>) {
        match toggle {
            Toggle::HandsFree => self.hands_free = shortcut,
            Toggle::Show(crate::Scope::Screen) => self.show_screen = shortcut,
            Toggle::Show(crate::Scope::Window) => self.show_window = shortcut,
            Toggle::Show(crate::Scope::Area) => self.show_area = shortcut,
        }
    }
}

/// What a toggle shortcut starts and finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    HandsFree,
    Show(crate::Scope),
}

/// The shortcut type of the system Speakeasy runs on.
///
/// Linux records no shortcuts; it names Windows' type so shared Settings code type-checks there.
#[cfg(target_os = "macos")]
pub type NativeShortcut = MacShortcut;
/// The shortcut type of the system Speakeasy runs on.
///
/// Linux records no shortcuts; it names Windows' type so shared Settings code type-checks there.
#[cfg(not(target_os = "macos"))]
pub type NativeShortcut = WindowsShortcut;

/// What a shortcut recording reports as keys go down and up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// The modifiers held so far, if any.
    Holding(Option<NativeShortcut>),
    /// The keys pressed; Settings still checks them.
    Chosen(NativeShortcut),
    /// Esc alone ended the recording.
    Cancelled,
}

/// The talk shortcut on each desktop, which tells a Show's pointer when Shift draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shortcut {
    pub mac: MacShortcut,
    pub windows: WindowsShortcut,
}

/// One platform's shortcuts: hold to talk, and the optional toggles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shortcuts<S> {
    pub talk: S,
    pub toggles: Toggles<S>,
}

impl<S: Copy> Shortcuts<S> {
    /// Every shortcut set, hold to talk first.
    pub fn bindings(&self) -> impl Iterator<Item = (Binding, S)> {
        std::iter::once((Binding::Talk, self.talk)).chain(
            self.toggles
                .iter()
                .map(|(toggle, shortcut)| (Binding::Toggle(toggle), shortcut)),
        )
    }
}

/// What a shortcut does: hold to talk, or toggle an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    Talk,
    Toggle(Toggle),
}

/// Why keys cannot serve a binding beside the other shortcuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Problem(Problem),
    /// A toggle acts on a key press, so it needs a key besides modifiers.
    NeedsKey,
    /// Another binding has these keys.
    Taken(Binding),
    /// Holding the talk shortcut's modifiers, part of these keys, starts dictation first.
    BehindTalk,
    /// These talk keys are part of a toggle's, so the toggle would start dictation instead.
    Covers(Toggle),
}

static MAC: Layout = Layout {
    modifiers: [
        Some(Named {
            name: "control",
            label: "Control",
        }),
        Some(Named {
            name: "option",
            label: "Option",
        }),
        Some(Named {
            name: "shift",
            label: "Shift",
        }),
        Some(Named {
            name: "command",
            label: "Command",
        }),
        Some(Named {
            name: "function",
            label: "Fn",
        }),
    ],
    order: &[
        Modifier::Function,
        Modifier::Control,
        Modifier::Alt,
        Modifier::Shift,
        Modifier::Super,
    ],
    keys: MAC_KEYS,
    aliases: &[],
    typing: ModifierSet::SHIFT.with(Modifier::Alt),
    alt_graph: None,
    printed: cfg_select! {
        target_os = "macos" => crate::macos::printed_key,
        _ => unprinted,
    },
};

static WINDOWS: Layout = Layout {
    modifiers: [
        Some(Named {
            name: "control",
            label: "Ctrl",
        }),
        Some(Named {
            name: "alt",
            label: "Alt",
        }),
        Some(Named {
            name: "shift",
            label: "Shift",
        }),
        Some(Named {
            name: "windows",
            label: "Win",
        }),
        None,
    ],
    order: &[
        Modifier::Control,
        Modifier::Super,
        Modifier::Alt,
        Modifier::Shift,
    ],
    keys: WINDOWS_KEYS,
    // Versions through 0.12 saved the default under this one name, so every settings file they
    // wrote holds it.
    aliases: &[("control_windows", WindowsShortcut::CONTROL_WINDOWS.0)],
    typing: ModifierSet::SHIFT,
    alt_graph: Some(ModifierSet(Modifier::Control.bit() | Modifier::Alt.bit())),
    printed: cfg_select! {
        target_os = "windows" => crate::windows::printed_key,
        _ => unprinted,
    },
};

/// Where the system can't say what a key prints, the table's label stands.
const fn unprinted(_code: u16) -> Option<char> {
    None
}

/// ANSI positions, named by what US layouts print there.
const MAC_KEYS: &[Key] = &[
    key(0, "a", "A", Kind::Character),
    key(1, "s", "S", Kind::Character),
    key(2, "d", "D", Kind::Character),
    key(3, "f", "F", Kind::Character),
    key(4, "h", "H", Kind::Character),
    key(5, "g", "G", Kind::Character),
    key(6, "z", "Z", Kind::Character),
    key(7, "x", "X", Kind::Character),
    key(8, "c", "C", Kind::Character),
    key(9, "v", "V", Kind::Character),
    key(10, "section", "§", Kind::Character),
    key(11, "b", "B", Kind::Character),
    key(12, "q", "Q", Kind::Character),
    key(13, "w", "W", Kind::Character),
    key(14, "e", "E", Kind::Character),
    key(15, "r", "R", Kind::Character),
    key(16, "y", "Y", Kind::Character),
    key(17, "t", "T", Kind::Character),
    key(18, "1", "1", Kind::Character),
    key(19, "2", "2", Kind::Character),
    key(20, "3", "3", Kind::Character),
    key(21, "4", "4", Kind::Character),
    key(22, "6", "6", Kind::Character),
    key(23, "5", "5", Kind::Character),
    key(24, "equals", "=", Kind::Character),
    key(25, "9", "9", Kind::Character),
    key(26, "7", "7", Kind::Character),
    key(27, "minus", "-", Kind::Character),
    key(28, "8", "8", Kind::Character),
    key(29, "0", "0", Kind::Character),
    key(30, "right_bracket", "]", Kind::Character),
    key(31, "o", "O", Kind::Character),
    key(32, "u", "U", Kind::Character),
    key(33, "left_bracket", "[", Kind::Character),
    key(34, "i", "I", Kind::Character),
    key(35, "p", "P", Kind::Character),
    key(36, "return", "Return", Kind::Typing),
    key(37, "l", "L", Kind::Character),
    key(38, "j", "J", Kind::Character),
    key(39, "quote", "'", Kind::Character),
    key(40, "k", "K", Kind::Character),
    key(41, "semicolon", ";", Kind::Character),
    key(42, "backslash", "\\", Kind::Character),
    key(43, "comma", ",", Kind::Character),
    key(44, "slash", "/", Kind::Character),
    key(45, "n", "N", Kind::Character),
    key(46, "m", "M", Kind::Character),
    key(47, "period", ".", Kind::Character),
    key(48, "tab", "Tab", Kind::Typing),
    key(49, "space", "Space", Kind::Typing),
    key(50, "grave", "\u{60}", Kind::Character),
    key(51, "delete", "Delete", Kind::Typing),
    key(53, "escape", "Esc", Kind::Reserved),
    key(57, "caps_lock", "Caps Lock", Kind::Reserved),
    key(64, "f17", "F17", Kind::Alone),
    key(65, "keypad_decimal", "Keypad .", Kind::Typing),
    key(67, "keypad_multiply", "Keypad *", Kind::Typing),
    key(69, "keypad_plus", "Keypad +", Kind::Typing),
    key(71, "keypad_clear", "Clear", Kind::Typing),
    key(75, "keypad_divide", "Keypad /", Kind::Typing),
    key(76, "keypad_enter", "Enter", Kind::Typing),
    key(78, "keypad_minus", "Keypad -", Kind::Typing),
    key(79, "f18", "F18", Kind::Alone),
    key(80, "f19", "F19", Kind::Alone),
    key(81, "keypad_equals", "Keypad =", Kind::Typing),
    key(82, "keypad_0", "Keypad 0", Kind::Typing),
    key(83, "keypad_1", "Keypad 1", Kind::Typing),
    key(84, "keypad_2", "Keypad 2", Kind::Typing),
    key(85, "keypad_3", "Keypad 3", Kind::Typing),
    key(86, "keypad_4", "Keypad 4", Kind::Typing),
    key(87, "keypad_5", "Keypad 5", Kind::Typing),
    key(88, "keypad_6", "Keypad 6", Kind::Typing),
    key(89, "keypad_7", "Keypad 7", Kind::Typing),
    key(90, "f20", "F20", Kind::Alone),
    key(91, "keypad_8", "Keypad 8", Kind::Typing),
    key(92, "keypad_9", "Keypad 9", Kind::Typing),
    key(93, "jis_yen", "¥", Kind::Character),
    key(94, "jis_underscore", "_", Kind::Character),
    key(95, "keypad_comma", "Keypad ,", Kind::Typing),
    key(96, "f5", "F5", Kind::Alone),
    key(97, "f6", "F6", Kind::Alone),
    key(98, "f7", "F7", Kind::Alone),
    key(99, "f3", "F3", Kind::Alone),
    key(100, "f8", "F8", Kind::Alone),
    key(101, "f9", "F9", Kind::Alone),
    key(103, "f11", "F11", Kind::Alone),
    key(105, "f13", "F13", Kind::Alone),
    key(106, "f16", "F16", Kind::Alone),
    key(107, "f14", "F14", Kind::Alone),
    key(109, "f10", "F10", Kind::Alone),
    key(111, "f12", "F12", Kind::Alone),
    key(113, "f15", "F15", Kind::Alone),
    key(114, "help", "Help", Kind::Alone),
    key(115, "home", "Home", Kind::Typing),
    key(116, "page_up", "Page Up", Kind::Typing),
    key(117, "forward_delete", "Forward Delete", Kind::Typing),
    key(118, "f4", "F4", Kind::Alone),
    key(119, "end", "End", Kind::Typing),
    key(120, "f2", "F2", Kind::Alone),
    key(121, "page_down", "Page Down", Kind::Typing),
    key(122, "f1", "F1", Kind::Alone),
    key(123, "left", "Left", Kind::Typing),
    key(124, "right", "Right", Kind::Typing),
    key(125, "down", "Down", Kind::Typing),
    key(126, "up", "Up", Kind::Typing),
];

const WINDOWS_KEYS: &[Key] = &[
    key(0x08, "backspace", "Backspace", Kind::Typing),
    key(0x09, "tab", "Tab", Kind::Typing),
    key(0x0D, "enter", "Enter", Kind::Typing),
    key(0x13, "pause", "Pause", Kind::Alone),
    key(0x14, "caps_lock", "Caps Lock", Kind::Reserved),
    key(0x1B, "escape", "Esc", Kind::Reserved),
    key(0x20, "space", "Space", Kind::Typing),
    key(0x21, "page_up", "Page Up", Kind::Typing),
    key(0x22, "page_down", "Page Down", Kind::Typing),
    key(0x23, "end", "End", Kind::Typing),
    key(0x24, "home", "Home", Kind::Typing),
    key(0x25, "left", "Left", Kind::Typing),
    key(0x26, "up", "Up", Kind::Typing),
    key(0x27, "right", "Right", Kind::Typing),
    key(0x28, "down", "Down", Kind::Typing),
    key(0x2C, "print_screen", "Print Screen", Kind::Alone),
    key(0x2D, "insert", "Insert", Kind::Alone),
    key(0x2E, "delete", "Delete", Kind::Typing),
    key(0x30, "0", "0", Kind::Character),
    key(0x31, "1", "1", Kind::Character),
    key(0x32, "2", "2", Kind::Character),
    key(0x33, "3", "3", Kind::Character),
    key(0x34, "4", "4", Kind::Character),
    key(0x35, "5", "5", Kind::Character),
    key(0x36, "6", "6", Kind::Character),
    key(0x37, "7", "7", Kind::Character),
    key(0x38, "8", "8", Kind::Character),
    key(0x39, "9", "9", Kind::Character),
    key(0x41, "a", "A", Kind::Character),
    key(0x42, "b", "B", Kind::Character),
    key(0x43, "c", "C", Kind::Character),
    key(0x44, "d", "D", Kind::Character),
    key(0x45, "e", "E", Kind::Character),
    key(0x46, "f", "F", Kind::Character),
    key(0x47, "g", "G", Kind::Character),
    key(0x48, "h", "H", Kind::Character),
    key(0x49, "i", "I", Kind::Character),
    key(0x4A, "j", "J", Kind::Character),
    key(0x4B, "k", "K", Kind::Character),
    key(0x4C, "l", "L", Kind::Character),
    key(0x4D, "m", "M", Kind::Character),
    key(0x4E, "n", "N", Kind::Character),
    key(0x4F, "o", "O", Kind::Character),
    key(0x50, "p", "P", Kind::Character),
    key(0x51, "q", "Q", Kind::Character),
    key(0x52, "r", "R", Kind::Character),
    key(0x53, "s", "S", Kind::Character),
    key(0x54, "t", "T", Kind::Character),
    key(0x55, "u", "U", Kind::Character),
    key(0x56, "v", "V", Kind::Character),
    key(0x57, "w", "W", Kind::Character),
    key(0x58, "x", "X", Kind::Character),
    key(0x59, "y", "Y", Kind::Character),
    key(0x5A, "z", "Z", Kind::Character),
    key(0x5D, "menu", "Menu", Kind::Alone),
    key(0x60, "numpad_0", "Num 0", Kind::Typing),
    key(0x61, "numpad_1", "Num 1", Kind::Typing),
    key(0x62, "numpad_2", "Num 2", Kind::Typing),
    key(0x63, "numpad_3", "Num 3", Kind::Typing),
    key(0x64, "numpad_4", "Num 4", Kind::Typing),
    key(0x65, "numpad_5", "Num 5", Kind::Typing),
    key(0x66, "numpad_6", "Num 6", Kind::Typing),
    key(0x67, "numpad_7", "Num 7", Kind::Typing),
    key(0x68, "numpad_8", "Num 8", Kind::Typing),
    key(0x69, "numpad_9", "Num 9", Kind::Typing),
    key(0x6A, "numpad_multiply", "Num *", Kind::Typing),
    key(0x6B, "numpad_add", "Num +", Kind::Typing),
    key(0x6D, "numpad_subtract", "Num -", Kind::Typing),
    key(0x6E, "numpad_decimal", "Num .", Kind::Typing),
    key(0x6F, "numpad_divide", "Num /", Kind::Typing),
    key(0x70, "f1", "F1", Kind::Alone),
    key(0x71, "f2", "F2", Kind::Alone),
    key(0x72, "f3", "F3", Kind::Alone),
    key(0x73, "f4", "F4", Kind::Alone),
    key(0x74, "f5", "F5", Kind::Alone),
    key(0x75, "f6", "F6", Kind::Alone),
    key(0x76, "f7", "F7", Kind::Alone),
    key(0x77, "f8", "F8", Kind::Alone),
    key(0x78, "f9", "F9", Kind::Alone),
    key(0x79, "f10", "F10", Kind::Alone),
    key(0x7A, "f11", "F11", Kind::Alone),
    key(0x7B, "f12", "F12", Kind::Alone),
    key(0x7C, "f13", "F13", Kind::Alone),
    key(0x7D, "f14", "F14", Kind::Alone),
    key(0x7E, "f15", "F15", Kind::Alone),
    key(0x7F, "f16", "F16", Kind::Alone),
    key(0x80, "f17", "F17", Kind::Alone),
    key(0x81, "f18", "F18", Kind::Alone),
    key(0x82, "f19", "F19", Kind::Alone),
    key(0x83, "f20", "F20", Kind::Alone),
    key(0x84, "f21", "F21", Kind::Alone),
    key(0x85, "f22", "F22", Kind::Alone),
    key(0x86, "f23", "F23", Kind::Alone),
    key(0x87, "f24", "F24", Kind::Alone),
    key(0x90, "num_lock", "Num Lock", Kind::Reserved),
    key(0x91, "scroll_lock", "Scroll Lock", Kind::Alone),
    key(0xBA, "semicolon", ";", Kind::Character),
    key(0xBB, "equals", "=", Kind::Character),
    key(0xBC, "comma", ",", Kind::Character),
    key(0xBD, "minus", "-", Kind::Character),
    key(0xBE, "period", ".", Kind::Character),
    key(0xBF, "slash", "/", Kind::Character),
    key(0xC0, "grave", "\u{60}", Kind::Character),
    key(0xDB, "left_bracket", "[", Kind::Character),
    key(0xDC, "backslash", "\\", Kind::Character),
    key(0xDD, "right_bracket", "]", Kind::Character),
    key(0xDE, "quote", "'", Kind::Character),
    key(0xDF, "oem_8", "`", Kind::Character),
    key(0xE2, "oem_102", "\\", Kind::Character),
];

#[cfg(test)]
mod tests;
