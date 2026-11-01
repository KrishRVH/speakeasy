use proptest::prelude::*;

use super::*;

#[test]
fn punctuation_separates_words_and_apostrophes_join_them() {
    let text = "  'DON’T'__stop—api's...now! ’ ' ";
    for (case, expected) in [
        (Case::Camel, "dontStopApisNow"),
        (Case::Pascal, "DontStopApisNow"),
        (Case::Snake, "dont_stop_apis_now"),
        (Case::Kebab, "dont-stop-apis-now"),
        (Case::Constant, "DONT_STOP_APIS_NOW"),
    ] {
        assert_eq!(Style::Identifier(case).apply(text.into()), expected);
        assert_eq!(Style::Identifier(case).apply("—’''_ \n🙂".into()), "");
    }
}

#[test]
fn unicode_case_expansions_keep_letters_without_introducing_separators() {
    for (case, text, expected) in [
        (Case::Camel, "ÉLAN naïve Straße", "élanNaïveStraße"),
        (Case::Pascal, "ÉLAN naïve Straße", "ÉlanNaïveStraße"),
        (Case::Snake, "ÉLAN naïve Straße", "élan_naïve_straße"),
        (Case::Constant, "ÉLAN naïve Straße", "ÉLAN_NAÏVE_STRASSE"),
        (Case::Pascal, "ßeta İSTANBUL", "SSetaİstanbul"),
        (Case::Snake, "İSTANBUL", "istanbul"),
        (Case::Constant, "ǰ 日本語 世界", "J_日本語_世界"),
        (Case::Kebab, "日本語 世界", "日本語-世界"),
    ] {
        assert_eq!(Style::Identifier(case).apply(text.into()), expected);
    }
}

#[test]
fn digits_keep_their_spoken_boundaries_and_never_gain_a_prefix() {
    for (case, text, expected) in [
        (Case::Camel, "version 2 API", "version2Api"),
        (Case::Pascal, "version 2 API", "Version2Api"),
        (Case::Snake, "version 2 API", "version_2_api"),
        (Case::Camel, "v2 API", "v2Api"),
        (Case::Snake, "v2 API", "v2_api"),
        (Case::Camel, "42 bottles", "42Bottles"),
        (Case::Snake, "42 bottles", "42_bottles"),
        (Case::Constant, "3d ٢", "3D_٢"),
    ] {
        assert_eq!(Style::Identifier(case).apply(text.into()), expected);
    }
}

#[test]
fn prose_and_verbatim_leave_formatting_to_the_existing_cleanup_pipeline() {
    let text = "\tUm, I’m keeping  ÉLAN’s punctuation!\n";
    for style in [Style::Prose, Style::Verbatim] {
        assert_eq!(style.apply(text.into()), text);
    }
    assert_eq!(Style::Verbatim.fillers(Fillers::Remove), Fillers::Preserve);
    assert_eq!(Style::Prose.fillers(Fillers::Remove), Fillers::Remove);
    assert_eq!(Style::Prose.fillers(Fillers::Preserve), Fillers::Preserve);
}

#[test]
fn wire_styles_are_flat_cli_names_and_reject_unrecognized_values() -> anyhow::Result<()> {
    for (wire, style) in [
        ("\"prose\"", Style::Prose),
        ("\"verbatim\"", Style::Verbatim),
        ("\"camel\"", Style::Identifier(Case::Camel)),
        ("\"pascal\"", Style::Identifier(Case::Pascal)),
        ("\"snake\"", Style::Identifier(Case::Snake)),
        ("\"kebab\"", Style::Identifier(Case::Kebab)),
        ("\"constant\"", Style::Identifier(Case::Constant)),
    ] {
        assert_eq!(serde_json::to_string(&style)?, wire);
        assert_eq!(serde_json::from_str::<Style>(wire)?, style);
    }
    for wire in ["null", "42", "\"unknown\"", "{\"identifier\":\"camel\"}"] {
        assert!(serde_json::from_str::<Style>(wire).is_err());
    }
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_unicode_produces_normalized_identifier_boundaries(
        text in proptest::collection::vec(any::<char>(), 0..128),
    ) {
        let text: String = text.into_iter().collect();
        for (case, separator) in [
            (Case::Camel, None),
            (Case::Pascal, None),
            (Case::Snake, Some('_')),
            (Case::Kebab, Some('-')),
            (Case::Constant, Some('_')),
        ] {
            let style = Style::Identifier(case);
            let output = style.apply(text.clone());
            prop_assert!(output.chars().all(|character| {
                character.is_alphanumeric() || Some(character) == separator
            }), "identifier alphabet");
            if let Some(separator) = separator {
                prop_assert!(!output.starts_with(separator));
                prop_assert!(!output.ends_with(separator));
                prop_assert!(!output.chars().zip(output.chars().skip(1)).any(|(left, right)| {
                    left == separator && right == separator
                }), "adjacent separators");
                prop_assert_eq!(style.apply(output.clone()), output);
            }
        }
    }
}
