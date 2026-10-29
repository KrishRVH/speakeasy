//! Alignment cases on hand-timed narration.

use super::*;
use crate::recording::tests::words;

fn at(anchors: &[u64]) -> Vec<Moment> {
    anchors.iter().copied().map(Moment::from_millis).collect()
}

/// Each keyframe's text, empty where nothing was said.
fn texts(anchors: &[u64], narration: &Narration) -> Vec<String> {
    align(&at(anchors), narration)
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect()
}

const HEADER_THEN_TAP: &[(u64, &str)] = &[
    (100, "First"),
    (300, "the"),
    (500, "header."),
    (2_000, "When"),
    (2_200, "I"),
    (2_400, "tap"),
    (2_600, "it,"),
    (2_800, "nothing."),
];

#[test]
fn words_partition_without_duplicates_or_gaps() {
    let said = words(&[
        (0, "Make"),
        (1_000, "this"),
        (2_000, "smaller"),
        (2_100, ","),
        (3_000, "and"),
        (4_000, "move"),
        (5_500, "that."),
    ]);
    assert_eq!(
        texts(&[1_000, 4_000], &said),
        ["Make this smaller,", "and move that."]
    );
}

#[test]
fn an_action_mid_sentence_takes_the_sentence_from_its_start() {
    let said = words(HEADER_THEN_TAP);
    assert_eq!(
        texts(&[400, 2_450], &said),
        ["First the header.", "When I tap it, nothing."]
    );
}

#[test]
fn a_short_comma_lead_in_stays_with_its_clause() {
    let said = words(&[
        (200, "The"),
        (400, "message"),
        (600, "was"),
        (800, "received."),
        (2_000, "Um,"),
        (2_300, "or"),
        (2_500, "it"),
        (2_700, "should"),
        (2_900, "appear."),
    ]);
    assert_eq!(
        texts(&[500, 2_600], &said),
        ["The message was received.", "Um, or it should appear."]
    );
}

#[test]
fn a_long_pause_starts_a_new_thought_even_without_punctuation() {
    // Recognizers stretch a word's end across the pause after it, so only starts count.
    let said = Narration::Words(vec![
        Word {
            span: Span::new(Moment::from_millis(0), Moment::from_millis(2_900)),
            text: "first".into(),
        },
        Word {
            span: Span::new(Moment::from_millis(3_000), Moment::from_millis(3_200)),
            text: "second".into(),
        },
    ]);
    assert_eq!(texts(&[0, 3_100], &said), ["first", "second"]);
}

#[test]
fn an_action_after_a_finished_sentence_does_not_reopen_it() {
    // The recognizer stretched "header." across the pause after it; an action during that
    // pause belongs to what comes next.
    let mut said = words(HEADER_THEN_TAP).words().to_vec();
    said[2].span = Span::new(Moment::from_millis(500), Moment::from_millis(1_950));
    assert_eq!(
        texts(&[0, 1_800], &Narration::Words(said)),
        ["First the header.", "When I tap it, nothing."]
    );
}

#[test]
fn untimed_text_belongs_to_the_first_keyframe_and_silence_to_none() {
    let text = Narration::Text(" All of it. ".into());
    assert_eq!(
        align(&at(&[0, 900]), &text),
        [Some("All of it.".to_owned()), None]
    );
    assert_eq!(align(&at(&[0]), &Narration::default()), [None]);
    assert_eq!(align(&at(&[0]), &words(&[(0, "  ")])), [None]);
    assert_eq!(align(&[], &text), Vec::<Option<String>>::new());
}
