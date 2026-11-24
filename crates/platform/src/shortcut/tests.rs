use proptest::prelude::*;

use super::*;

#[test]
fn every_saved_shortcut_from_earlier_versions_still_reads() {
    for (saved, shortcut, label) in [
        ("function", MacShortcut::FUNCTION, "Fn"),
        ("right_option", MacShortcut::RIGHT_OPTION, "Right Option"),
        ("right_command", MacShortcut::RIGHT_COMMAND, "Right Command"),
        ("f13", MacShortcut::F13, "F13"),
    ] {
        assert_eq!(saved.parse(), Ok(shortcut), "{saved}");
        assert_eq!(shortcut.label(), label);
    }
    for (saved, shortcut, label) in [
        (
            "control_windows",
            WindowsShortcut::CONTROL_WINDOWS,
            "Ctrl + Win",
        ),
        (
            "right_control",
            WindowsShortcut::RIGHT_CONTROL,
            "Right Ctrl",
        ),
        ("f13", WindowsShortcut::F13, "F13"),
    ] {
        assert_eq!(saved.parse(), Ok(shortcut), "{saved}");
        assert_eq!(shortcut.label(), label);
    }
    assert_eq!(
        WindowsShortcut::CONTROL_WINDOWS.to_string(),
        "control+windows"
    );
}

#[test]
fn recorded_keys_read_back_as_written_and_label_in_each_platforms_words() {
    let held = |modifiers: &[Modifier]| {
        modifiers
            .iter()
            .fold(ModifierSet::EMPTY, |set, modifier| set.with(*modifier))
    };
    let chord = |modifiers: &[Modifier], key| {
        Chord::recorded(held(modifiers), None, key).expect("keys were held")
    };
    let windows = WindowsShortcut::new(chord(&[Modifier::Shift, Modifier::Control], Some(0x4B)));
    assert_eq!(windows.to_string(), "control+shift+k");
    assert_eq!(windows.label(), "Ctrl + Shift + K");
    let mac = MacShortcut::new(chord(&[Modifier::Super, Modifier::Alt], Some(49)));
    assert_eq!(mac.to_string(), "option+command+space");
    assert_eq!(mac.label(), "Option + Command + Space");
    let media = MacShortcut::new(chord(&[], Some(160)));
    assert_eq!(media.to_string(), "key_160");
    assert_eq!("key_160".parse(), Ok(media));
    assert_eq!(
        media.problem(),
        None,
        "A key Speakeasy can't name types nothing"
    );
}

#[test]
fn a_lone_modifier_keeps_its_side_and_any_other_chord_matches_both() {
    let right = Chord::recorded(
        ModifierSet::EMPTY.with(Modifier::Alt),
        Some((Modifier::Alt, Side::Right)),
        None,
    );
    assert_eq!(right.map(MacShortcut::new), Some(MacShortcut::RIGHT_OPTION));
    let with_key = Chord::recorded(
        ModifierSet::EMPTY.with(Modifier::Alt),
        Some((Modifier::Alt, Side::Right)),
        Some(49),
    );
    assert_eq!(with_key.map(Chord::side), Some(None));
    let function = Chord::recorded(
        ModifierSet::EMPTY.with(Modifier::Function),
        Some((Modifier::Function, Side::Left)),
        None,
    );
    assert_eq!(function.map(MacShortcut::new), Some(MacShortcut::FUNCTION));
}

#[test]
fn keys_that_type_cancel_or_shift_cannot_be_shortcuts_alone() {
    let parse = |text: &str| {
        text.parse::<WindowsShortcut>()
            .map(WindowsShortcut::problem)
    };
    assert_eq!(parse("k"), Ok(Some(Problem::Types)));
    assert_eq!(
        parse("shift+k"),
        Ok(Some(Problem::Types)),
        "Shift+K types a capital"
    );
    assert_eq!(parse("control+k"), Ok(None));
    assert_eq!(parse("escape"), Ok(Some(Problem::Reserved)));
    assert_eq!(parse("control+escape"), Ok(Some(Problem::Reserved)));
    assert_eq!(parse("right_shift"), Ok(Some(Problem::ShiftAlone)));
    assert_eq!(parse("f13"), Ok(None));
    assert_eq!(parse("pause"), Ok(None));
    assert!(
        parse("control+k+j").is_err(),
        "A shortcut has one key at most"
    );
    assert!(parse("function").is_err(), "Windows has no Fn");
    assert!("control+nope".parse::<MacShortcut>().is_err());
}

#[test]
fn a_shortcut_starting_within_another_would_win_the_race_to_its_modifiers() {
    let parse = |text: &str| text.parse::<WindowsShortcut>().unwrap_or_default();
    let talk = WindowsShortcut::CONTROL_WINDOWS;
    assert!(talk.starts_within(parse("control+windows+h")));
    assert!(!talk.starts_within(parse("control+alt+h")));
    assert!(
        WindowsShortcut::F13.starts_within(parse("control+f13")),
        "F13 alone claims its key under any modifiers"
    );
    assert!(parse("control+space").starts_within(parse("control+shift+space")));
    assert!(!WindowsShortcut::F13.starts_within(parse("control+f14")));
    assert!(
        !WindowsShortcut::RIGHT_CONTROL.starts_within(parse("control+alt+h")),
        "Left Ctrl stays free for the other shortcut"
    );
    assert!(MacShortcut::FUNCTION.starts_within("function+f5".parse().unwrap_or_default()));
}

#[test]
fn shortcuts_refuse_keys_another_binding_has_or_that_one_would_start_first() {
    let parse = |text: &str| text.parse::<WindowsShortcut>().expect("a valid shortcut");
    let mut shortcuts = Shortcuts {
        talk: WindowsShortcut::CONTROL_WINDOWS,
        toggles: Toggles::default(),
    };
    let hands_free = Binding::Toggle(Toggle::HandsFree);
    let screen = Binding::Toggle(Toggle::Show(crate::Scope::Screen));
    assert_eq!(
        shortcuts.refusal(hands_free, parse("control+shift+h")),
        None
    );
    assert_eq!(
        shortcuts.refusal(hands_free, parse("control+windows+h")),
        Some(Refusal::BehindTalk)
    );
    assert_eq!(
        shortcuts.refusal(hands_free, parse("right_alt")),
        Some(Refusal::NeedsKey)
    );
    assert_eq!(
        shortcuts.refusal(hands_free, parse("h")),
        Some(Refusal::Problem(Problem::Types))
    );
    shortcuts.toggles.hands_free = Some(parse("control+shift+h"));
    assert_eq!(
        shortcuts.refusal(screen, parse("control+shift+h")),
        Some(Refusal::Taken(hands_free))
    );
    assert_eq!(
        shortcuts.refusal(Binding::Talk, parse("control+shift")),
        Some(Refusal::Covers(Toggle::HandsFree)),
        "Hold to talk would start before the hands-free key"
    );
    assert_eq!(shortcuts.refusal(Binding::Talk, WindowsShortcut::F13), None);
    assert_eq!(shortcuts.refused(), None);
    shortcuts.toggles.show_screen = shortcuts.toggles.hands_free;
    assert_eq!(
        shortcuts.refused(),
        Some((hands_free, Refusal::Taken(screen)))
    );

    let f13 = Shortcuts {
        talk: WindowsShortcut::F13,
        toggles: Toggles::default(),
    };
    assert_eq!(
        f13.refusal(hands_free, parse("control+f13")),
        Some(Refusal::BehindTalk),
        "F13 would take its key from the toggle"
    );
}

#[test]
fn a_character_key_refuses_the_modifiers_that_still_type_with_it() {
    let windows = |text: &str| {
        text.parse::<WindowsShortcut>()
            .map(WindowsShortcut::problem)
    };
    assert_eq!(
        windows("control+alt+s"),
        Ok(Some(Problem::AltGraph)),
        "AltGr+S types ś on Polish layouts"
    );
    assert_eq!(windows("control+alt+shift+2"), Ok(Some(Problem::AltGraph)));
    assert_eq!(
        windows("control+alt"),
        Ok(Some(Problem::AltGraph)),
        "AltGr alone reads as Ctrl and Alt"
    );
    assert_eq!(windows("control+alt+f5"), Ok(None));
    assert_eq!(windows("control+windows+alt+s"), Ok(None));
    assert_eq!(
        windows("oem_8"),
        Ok(Some(Problem::Types)),
        "UK layouts type a backtick on this key"
    );
    let mac = |text: &str| text.parse::<MacShortcut>().map(MacShortcut::problem);
    assert_eq!(
        mac("option+s"),
        Ok(Some(Problem::Types)),
        "Option+S types ß"
    );
    assert_eq!(mac("option+shift+jis_yen"), Ok(Some(Problem::Types)));
    assert_eq!(mac("option+space"), Ok(None));
    assert_eq!(mac("control+option+s"), Ok(None));
    assert_eq!(mac("control+option"), Ok(None), "Macs have no AltGr");
}

#[test]
fn a_character_key_shows_what_the_layout_prints_and_a_digit_keeps_its_digit() {
    let azerty = Layout {
        printed: |code| match code {
            12 => Some('a'),
            1 => Some('ß'),
            18 => Some('&'),
            _ => None,
        },
        ..MAC
    };
    let label = |key| {
        Chord::recorded(ModifierSet::EMPTY.with(Modifier::Super), None, Some(key))
            .map(|chord| chord.label(&azerty))
    };
    assert_eq!(
        label(12).as_deref(),
        Some("Command + A"),
        "The Q position prints A on AZERTY"
    );
    assert_eq!(label(1).as_deref(), Some("Command + ß"));
    assert_eq!(label(18).as_deref(), Some("Command + 1"));
    assert_eq!(label(49).as_deref(), Some("Command + Space"));
}

fn mac_chord() -> impl Strategy<Value = MacShortcut> {
    let keys = prop::option::of(prop::sample::select(
        MAC_KEYS
            .iter()
            .map(|key| key.code)
            .chain([160, 179])
            .collect::<Vec<_>>(),
    ));
    (0_u8..32, keys, any::<bool>()).prop_filter_map("no keys", |(bits, key, right)| {
        let modifiers = Modifier::ALL
            .into_iter()
            .filter(|modifier| bits & modifier.bit() != 0)
            .fold(ModifierSet::EMPTY, ModifierSet::with);
        let side = if right { Side::Right } else { Side::Left };
        let sided = modifiers.single().map(|modifier| (modifier, side));
        Chord::recorded(modifiers, sided, key).map(MacShortcut::new)
    })
}

proptest! {
    #[test]
    fn every_recordable_mac_shortcut_saves_and_reads_back(shortcut in mac_chord()) {
        prop_assert_eq!(shortcut.to_string().parse::<MacShortcut>(), Ok(shortcut));
    }
}
