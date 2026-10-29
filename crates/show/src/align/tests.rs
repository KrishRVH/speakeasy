//! Alignment cases on hand-timed narration and on stretches of real narration: Scrawl's public
//! recordings, trimmed to the words and marks beside this file in `show.json`'s shape.

use serde::Deserialize;

use super::*;
use crate::{Mark, recording::tests::words};

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

/// The narration of a real recording, word by word as its recognizer timed it, and the marks
/// drawn meanwhile.
#[derive(Deserialize)]
struct Recorded {
    narration: Narration,
    marks: Vec<Mark>,
}

impl Recorded {
    fn parse(json: &str) -> Self {
        serde_json::from_str(json).unwrap()
    }

    /// Each mark's text when every mark has a keyframe of its own, anchored where its ink began,
    /// as the recording gave each of these marks a frame of its own.
    fn said_per_mark(&self) -> Vec<String> {
        let anchors: Vec<u64> = self
            .marks
            .iter()
            .map(|mark| mark.span.start().as_millis())
            .collect();
        texts(&anchors, &self.narration)
    }
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
fn ink_drawn_in_a_pause_mid_sentence_takes_the_sentence() {
    // The recognizer folded each pause into the word before it: "There's" lasts 2.6 s and the
    // second "and" 2 s, and the ink began before the next word.
    assert_eq!(
        Recorded::parse(include_str!("hex-motion.json")).said_per_mark(),
        [
            "Oh, that's nice. It's faster.",
            "Um, There's some jumps, though, in the animation."
        ]
    );
    assert_eq!(
        Recorded::parse(include_str!("hex-shout2.json")).said_per_mark(),
        [
            "Uh, and the reflection needs to be better,",
            "and the eyes."
        ]
    );
}

#[test]
fn ink_drawn_just_before_speaking_takes_what_is_said_next() {
    // The underline was drawn during "Um,". Speakeasy removes fillers by default, which leaves
    // the underline in the silence before the clause the filler opened, and the next mark inside
    // that clause.
    let mut recorded = Recorded::parse(include_str!("server-onboarding.json"));
    let before =
        "Also, this thing shouldn't be floating on the bottom. It covers up too much space.";
    let after = "and I like the right side or something. We take up the full window.";
    assert_eq!(
        recorded.said_per_mark(),
        [before, "Um, be way more minimal", after]
    );
    recorded.narration = Narration::Words(
        recorded
            .narration
            .words()
            .iter()
            .filter(|word| word.text != "Um,")
            .cloned()
            .collect(),
    );
    assert_eq!(
        recorded.said_per_mark(),
        [before, "be way more minimal", after]
    );
}

#[test]
fn ink_drawn_long_before_speaking_leaves_the_words_to_ink_drawn_while_saying_them() {
    let said = words(&[(3_000, "This"), (3_200, "one"), (3_400, "flickers.")]);
    assert_eq!(texts(&[0, 3_300], &said), ["", "This one flickers."]);
}

#[test]
fn a_sentence_left_hanging_is_over_once_the_silence_runs_long() {
    let said = words(&[
        (0, "That's"),
        (200, "done."),
        (1_000, "So"),
        (1_200, "the"),
        (9_000, "Next"),
        (9_200, "thing."),
    ]);
    assert_eq!(
        texts(&[0, 6_000], &said),
        ["That's done. So the", "Next thing."]
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
