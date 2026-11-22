use crate::{Moment, Narration, recording::tests::words, timed_clauses};

#[test]
fn pauses_and_punctuation_keep_real_starts_without_inferred_holding_times() {
    let Narration::Words(words) = words(&[
        (2_000, ")"),
        (0, " Done. "),
        (2_100, "Next"),
        (2_300, "move"),
        (4_500, "here"),
        (4_700, "."),
        (9_000, "  "),
    ]) else {
        panic!("The fixture is timed");
    };
    let rows = timed_clauses(&words);
    assert_eq!(
        rows.iter()
            .map(|row| (row.at.as_millis(), row.text.as_str()))
            .collect::<Vec<_>>(),
        [(0, "Done.)"), (2_100, "Next move"), (4_500, "here.")]
    );
}

#[test]
fn uninterrupted_speech_starts_a_new_row_after_ten_seconds() {
    let spoken: Vec<_> = (0..=10_000).step_by(1_000).map(|at| (at, "word")).collect();
    let narration = words(&spoken);
    let rows = timed_clauses(narration.words());
    assert_eq!(
        rows.iter().map(|row| row.at).collect::<Vec<_>>(),
        [Moment::START, Moment::from_millis(10_000)]
    );
}

#[test]
fn closing_quotes_and_ideographic_full_stops_keep_their_sentence() {
    for (fixture, expected) in [
        (
            &[(0, "Done."), (200, "\""), (400, "Next")][..],
            ["Done.\"", "Next"],
        ),
        (&[(0, "“Done. ” "), (400, "Next")][..], ["“Done. ”", "Next"]),
        (
            &[(0, "完了"), (200, "。"), (400, "次")][..],
            ["完了。", "次"],
        ),
    ] {
        let Narration::Words(words) = words(fixture) else {
            panic!("The fixture is timed");
        };
        let rows = timed_clauses(&words);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            expected,
            "Rows for {fixture:?}"
        );
    }
}
