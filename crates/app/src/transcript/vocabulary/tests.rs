use speakeasy_show::{Moment, Span};

use super::*;

fn rules() -> Vec<Correction> {
    [
        ("cube", "Kube"),
        ("cube control", "kubectl"),
        ("kubectl", "DO NOT CHAIN"),
        ("élan", "Élan"),
    ]
    .map(|(heard, written)| Correction {
        heard: heard.into(),
        written: written.into(),
    })
    .to_vec()
}

#[test]
fn longest_phrase_preserves_edges_without_rewriting_identifiers_or_chaining() {
    let rules = rules();
    let vocabulary = Vocabulary::new(&rules);
    for (input, expected) in [
        (
            "Use (CUBE control), then cube.",
            "Use (kubectl), then Kube.",
        ),
        (
            "cube, control cube-control my_cube node.cube cube2",
            "Kube, control cube-control my_cube node.cube cube2",
        ),
        ("cube 'control' ÉLAN!", "Kube 'control' Élan!"),
        ("中文cube cube中文", "中文cube cube中文"),
    ] {
        assert_eq!(vocabulary.text(input.to_owned()), expected);
    }
}

#[test]
fn a_phrase_preserves_union_of_word_times_including_overlaps() {
    let rules = rules();
    let vocabulary = Vocabulary::new(&rules);
    let word = |text: &str, from, to| Word {
        text: text.into(),
        span: Span::new(Moment::from_millis(from), Moment::from_millis(to)),
    };
    let narration = vocabulary.narration(Narration::Words(vec![
        word("cube", 100, 900),
        word("control!", 300, 600),
        word("Next.", 1_000, 1_200),
    ]));
    let Narration::Words(words) = narration else {
        panic!("Timing was lost");
    };
    assert_eq!(words[0], word("kubectl!", 100, 900));
    assert_eq!(words[1], word("Next.", 1_000, 1_200));
}

#[test]
fn invalid_and_duplicate_rules_are_refused() {
    let rule = |heard: &str, written: &str| Correction {
        heard: heard.into(),
        written: written.into(),
    };
    for rules in [
        vec![rule("two\nlines", "written")],
        vec![rule("heard", " ")],
        vec![rule("heard", "written"); LIMIT + 1],
        vec![rule(" spoken phrase ", "a"), rule("SPOKEN  PHRASE", "b")],
    ] {
        assert!(validate(&rules).is_err());
    }
    assert!(validate(&rules()).is_ok());
}
