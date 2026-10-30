//! Corrections defend exact phrase boundaries, punctuation, timing, and private diagnostics.

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
        assert_eq!(vocabulary.text(input), expected);
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
fn invalid_and_duplicate_rules_fail_without_echoing_their_contents() {
    for rules in [
        vec![Correction {
            heard: "PRIVATE\nPHRASE".into(),
            written: "secret".into(),
        }],
        vec![Correction {
            heard: "PRIVATE_PHRASE".into(),
            written: " ".into(),
        }],
        vec![
            Correction {
                heard: "PRIVATE_PHRASE".into(),
                written: "secret".into()
            };
            201
        ],
        vec![
            Correction {
                heard: " private phrase ".into(),
                written: "a".into(),
            },
            Correction {
                heard: "PRIVATE  PHRASE".into(),
                written: "b".into(),
            },
        ],
    ] {
        assert!(
            !validate(&rules)
                .unwrap_err()
                .to_string()
                .contains("PRIVATE")
        );
        assert!(!format!("{rules:?}").contains("PRIVATE"));
    }
    assert!(validate(&rules()).is_ok());
}
