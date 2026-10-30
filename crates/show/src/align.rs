//! Dividing the narration among keyframes.

use std::time::Duration;

use crate::{
    Moment, Span,
    recording::{Narration, Word},
};

/// Narration after a silence this long starts a new clause. Measured from one word's start to the
/// next because the recognizer stretches a word's end across the pause after it.
const PAUSE: Duration = Duration::from_secs(2);
/// A finished clause lasts at most this long past its last word's start; the recognizer's
/// reported end can run through the silence that follows.
const WORD_REACH: Duration = Duration::from_millis(700);
/// A clause left unfinished stays in progress through a pause until speech resumes, for up to
/// this long past its last word's start, and a clause beginning within that time resumes it:
/// people stop mid-sentence to draw what they mean, then go on.
const HESITATION: Duration = Duration::from_secs(4);
/// An anchor in silence may take a clause beginning up to this long after it: people mark or
/// click something just before saying what it is, and removing a filler such as "um" leaves
/// silence where the clause began.
const SPEECH_LAG: Duration = Duration::from_secs(1);
/// A word belongs to a keyframe whose text starts no later than this after it.
const ONSET_TOLERANCE: Duration = Duration::from_millis(50);
/// A comma ends a clause only after this many words, so a short lead-in such as "Um," stays with
/// the thought it opens.
const COMMA_CLAUSE_WORDS: usize = 3;

/// The words said for each keyframe, given each keyframe's anchor in keyframe order: the moment
/// its story begins, such as when its ink or action began or its cue was spoken.
///
/// A keyframe's clause is the one in progress at its anchor or, for an anchor in the silence just
/// before speech, the clause about to begin, unless the next anchor lies nearer that clause's
/// start: a click just before speaking leaves the words to ink drawn as they begin. A clause that
/// resumes one left hanging opens where that one opens. A keyframe's text starts where its clause
/// opens or, once an earlier keyframe's text holds that, where its clause began, or else at its
/// anchor, and always after the previous keyframe's text starts. Each text runs until the next
/// keyframe's text starts, so the narration is partitioned in order with nothing repeated or
/// dropped; punctuation the recognizer split from a word stays with that word. Narration before
/// the first keyframe belongs to it. Each text is trimmed, and a keyframe with nothing said gets
/// `None`.
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
            let mut owner = 0;
            for word in words {
                let text = word.text.trim();
                if !attaches(text) {
                    let onset = word.span.start().saturating_add(ONSET_TOLERANCE);
                    owner = starts
                        .iter()
                        .rposition(|start| *start <= onset)
                        .unwrap_or(0);
                }
                if let Some(keyframe) = said.get_mut(owner) {
                    append(keyframe, text);
                }
            }
        },
    }
    said.into_iter()
        .map(|text| Some(text).filter(|text| !text.is_empty()))
        .collect()
}

/// Words read as one clause.
struct Clause {
    /// From the clause's first word until it is over.
    span: Span,
    /// Where the clause opens: its first word or, when it resumes a clause left hanging, where
    /// that one opens.
    opening: Moment,
}

/// The clauses of `words`, in order.
fn clauses(words: &[&Word]) -> Vec<Clause> {
    let mut clauses: Vec<Clause> = Vec::new();
    let mut clause_words = 0;
    let mut previous: Option<&Word> = None;
    for &word in words {
        let start = word.span.start();
        let hanging_since = previous
            .replace(word)
            .filter(|previous| !ends_clause(&previous.text, clause_words))
            .map(|previous| previous.span.start());
        let reach = Span::new(start, word.span.end().min(start.saturating_add(WORD_REACH)));
        let opening = match (clauses.last_mut(), hanging_since) {
            (Some(clause), Some(last)) if start.since(last) < PAUSE => {
                clause.span = clause.span.union(reach);
                clause_words = clause_words.saturating_add(usize::from(word.is_spoken()));
                continue;
            },
            (Some(hanging), Some(last)) if start.since(last) <= HESITATION => {
                hanging.span = hanging.span.union(Span::at(start));
                hanging.opening
            },
            (Some(hanging), Some(last)) => {
                hanging.span = hanging
                    .span
                    .union(Span::at(last.saturating_add(HESITATION)));
                start
            },
            _ => start,
        };
        clauses.push(Clause {
            span: reach,
            opening,
        });
        clause_words = usize::from(word.is_spoken());
    }
    clauses
}

/// Where each keyframe's text starts, by the rules [`align`] gives.
fn text_starts(anchors: &[Moment], clauses: &[Clause]) -> Vec<Moment> {
    let mut starts: Vec<Moment> = Vec::with_capacity(anchors.len());
    let mut anchors = anchors.iter().copied().peekable();
    while let Some(anchor) = anchors.next() {
        let clause = clauses
            .iter()
            .find(|clause| clause.span.contains(anchor))
            .or_else(|| about_to_begin(clauses, anchor, anchors.peek().copied()));
        let earliest = starts.last().map_or(Moment::START, |previous| {
            previous.saturating_add(Duration::from_millis(1))
        });
        let start = clause
            .into_iter()
            .flat_map(|clause| [clause.opening, clause.span.start()])
            .chain([anchor])
            .find(|start| *start >= earliest)
            .unwrap_or(earliest);
        starts.push(start);
    }
    starts
}

/// The clause beginning up to [`SPEECH_LAG`] after `anchor`, which lies in silence, unless the
/// `next` anchor lies nearer that clause's start.
fn about_to_begin(clauses: &[Clause], anchor: Moment, next: Option<Moment>) -> Option<&Clause> {
    clauses
        .iter()
        .find(|clause| clause.span.start() > anchor)
        .filter(|clause| {
            let lead = clause.span.start().since(anchor);
            lead <= SPEECH_LAG && next.is_none_or(|next| next.distance(clause.span.start()) >= lead)
        })
}

fn ends_clause(text: &str, clause_words: usize) -> bool {
    let text = text.trim_end_matches(['"', '\'', '”', '’', ')', ']']);
    text.ends_with(['.', '!', '?', ';', ':'])
        || (clause_words >= COMMA_CLAUSE_WORDS && text.ends_with(','))
}

/// Whether `word` belongs to the word before it, as punctuation and closing marks do.
fn attaches(word: &str) -> bool {
    word.starts_with([',', '.', '!', '?', ';', ':', '’', '\'', ')', ']', '”'])
}

/// Joins a word to running text the way it was written: what [`attaches`] joins the word before,
/// and nothing follows an opening mark.
fn append(text: &mut String, word: &str) {
    if !text.is_empty() && !attaches(word) && !text.ends_with(['(', '[', '“']) {
        text.push(' ');
    }
    text.push_str(word);
}

#[cfg(test)]
mod tests;
