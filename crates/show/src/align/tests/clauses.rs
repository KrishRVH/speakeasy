//! Narration navigation preserves words and real onsets without exposing alignment's inferred
//! silence intervals as speech timing.

use proptest::prelude::*;

use crate::{Moment, Narration, Span, Word, recording::tests::words, timed_clauses};

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
    assert!(!format!("{rows:?}").contains("Next"));
}

proptest! {
    #[test]
    fn rows_partition_words_in_stable_time_order_and_bound_long_sentences(
        input in prop::collection::vec((0_u64..120_000, any::<bool>()), 0..120)
    ) {
        let words: Vec<_> = input.iter().enumerate().map(|(index, &(at, stop))| Word {
            span: Span::at(Moment::from_millis(at)),
            text: format!("w{index}{}", if stop { "." } else { "" }),
        }).collect();
        let mut expected: Vec<_> = words.iter().collect();
        expected.sort_by_key(|word| word.span.start());
        let rows = timed_clauses(&words);
        let actual: Vec<_> = rows.iter().flat_map(|row| row.text.split_whitespace()).collect();
        prop_assert_eq!(actual, expected.iter().map(|word| word.text.as_str()).collect::<Vec<_>>());
        for row in &rows {
            let members: Vec<_> = row.text.split_whitespace().map(|text|
                expected.iter().find(|word| word.text == text).unwrap()).collect();
            prop_assert_eq!(row.at, members.first().unwrap().span.start());
            prop_assert!(members.last().unwrap().span.start().since(row.at).as_millis() < 10_000);
        }
    }
}
