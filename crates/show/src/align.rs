//! Dividing the narration among keyframes.

use std::time::Duration;

use crate::{
    Moment,
    recording::{Narration, Word},
};

/// Narration after a silence this long starts a new thought. Measured from one word's start to the
/// next because the recognizer stretches a word's end across the pause after it.
const PAUSE: Duration = Duration::from_secs(2);
/// A clause lasts at most this long past its last word's start; the recognizer's reported end can
/// run through the silence that follows.
const WORD_REACH: Duration = Duration::from_millis(700);
/// A word belongs to a keyframe whose text starts no later than this after it.
const ONSET_TOLERANCE: Duration = Duration::from_millis(50);
/// A comma ends a clause only after this many words, so a short lead-in such as "Um," stays with
/// the thought it opens.
const COMMA_CLAUSE_WORDS: usize = 3;

/// The words said for each keyframe, given each keyframe's anchor: the moment its action began
/// or its cue was spoken, in keyframe order.
///
/// Each keyframe's text runs from the start of the clause in progress at its anchor until the
/// next keyframe's text starts, so the narration is partitioned in order with nothing repeated or
/// dropped. Narration before the first keyframe belongs to it.
pub(crate) fn align(anchors: &[Moment], narration: &Narration) -> Vec<Option<String>> {
    let mut said = vec![String::new(); anchors.len()];
    match narration {
        Narration::Text(text) => {
            if let Some(first) = said.first_mut() {
                text.trim().clone_into(first);
            }
        },
        Narration::Words(words) => {
            let mut words: Vec<&Word> = words
                .iter()
                .filter(|word| !word.text.trim().is_empty())
                .collect();
            words.sort_by_key(|word| word.span.start());
            let starts = text_starts(anchors, &clauses(&words));
            for word in words {
                let onset = word.span.start().saturating_add(ONSET_TOLERANCE);
                let owner = starts
                    .iter()
                    .rposition(|start| *start <= onset)
                    .unwrap_or(0);
                if let Some(text) = said.get_mut(owner) {
                    append(text, word.text.trim());
                }
            }
        },
    }
    said.into_iter()
        .map(|text| Some(text).filter(|text| !text.is_empty()))
        .collect()
}

/// Spans of words that read as one clause.
fn clauses(words: &[&Word]) -> Vec<(Moment, Moment)> {
    let mut clauses: Vec<(Moment, Moment)> = Vec::new();
    let mut clause_words = 0;
    let mut previous: Option<&Word> = None;
    for word in words {
        let continues = previous.is_some_and(|previous| {
            !ends_clause(&previous.text, clause_words)
                && word.span.start().since(previous.span.start()) < PAUSE
        });
        let end = word
            .span
            .end()
            .min(word.span.start().saturating_add(WORD_REACH));
        match clauses.last_mut() {
            Some(clause) if continues => {
                clause.1 = clause.1.max(end);
                clause_words = clause_words.saturating_add(usize::from(word.is_spoken()));
            },
            _ => {
                clauses.push((word.span.start(), end));
                clause_words = usize::from(word.is_spoken());
            },
        }
        previous = Some(word);
    }
    clauses
}

/// Where each keyframe's text starts: the start of the clause holding its anchor. When two
/// keyframes fall in one clause, the later one takes the words from its own moment.
fn text_starts(anchors: &[Moment], clauses: &[(Moment, Moment)]) -> Vec<Moment> {
    let mut starts: Vec<Moment> = Vec::with_capacity(anchors.len());
    for &anchor in anchors {
        let mut start = clauses
            .iter()
            .find(|(start, end)| *start <= anchor && anchor <= *end)
            .map_or(anchor, |clause| clause.0);
        if let Some(&previous) = starts.last()
            && start <= previous
        {
            start = anchor.max(previous.saturating_add(Duration::from_millis(1)));
        }
        starts.push(start);
    }
    starts
}

fn ends_clause(text: &str, clause_words: usize) -> bool {
    let text = text.trim_end_matches(['"', '\'', '”', '’', ')', ']']);
    text.ends_with(['.', '!', '?', ';', ':'])
        || (clause_words >= COMMA_CLAUSE_WORDS && text.ends_with(','))
}

/// Joins a word to running text the way it was written: punctuation and closing marks attach to
/// the word before, and nothing follows an opening mark.
fn append(text: &mut String, word: &str) {
    let attached = word.starts_with([',', '.', '!', '?', ';', ':', '’', '\'', ')', ']', '”']);
    if !text.is_empty() && !attached && !text.ends_with(['(', '[', '“']) {
        text.push(' ');
    }
    text.push_str(word);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    fn narration(words: &[(u64, &str)]) -> Narration {
        Narration::Words(
            words
                .iter()
                .map(|&(start, text)| Word {
                    span: Span::new(
                        Moment::from_millis(start),
                        Moment::from_millis(start.saturating_add(200)),
                    ),
                    text: text.to_owned(),
                })
                .collect(),
        )
    }

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

    #[test]
    fn words_partition_without_duplicates_or_gaps() {
        let said = narration(&[
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
        let said = narration(&[
            (100, "First"),
            (300, "the"),
            (500, "header."),
            (2_000, "When"),
            (2_200, "I"),
            (2_400, "tap"),
            (2_600, "it,"),
            (2_800, "nothing."),
        ]);
        assert_eq!(
            texts(&[400, 2_450], &said),
            ["First the header.", "When I tap it, nothing."]
        );
    }

    #[test]
    fn a_short_comma_lead_in_stays_with_its_clause() {
        let said = narration(&[
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
        let mut said = narration(&[
            (100, "First"),
            (300, "the"),
            (500, "header."),
            (2_000, "When"),
            (2_200, "I"),
            (2_400, "tap"),
            (2_600, "it,"),
            (2_800, "nothing."),
        ]);
        if let Narration::Words(words) = &mut said {
            words[2].span = Span::new(Moment::from_millis(500), Moment::from_millis(1_950));
        }
        assert_eq!(
            texts(&[0, 1_800], &said),
            ["First the header.", "When I tap it, nothing."]
        );
    }

    #[test]
    fn untimed_text_and_silence() {
        let text = Narration::Text(" All of it. ".into());
        assert_eq!(texts(&[0, 900], &text), ["All of it.", ""]);
        assert_eq!(texts(&[0], &Narration::default()), [""]);
        assert_eq!(texts(&[], &text), Vec::<String>::new());
    }
}
