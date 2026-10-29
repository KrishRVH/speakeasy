//! Dividing the narration among keyframes.

use std::time::Duration;

use crate::{
    Moment, Span,
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

/// The words said for each keyframe, given each keyframe's anchor in keyframe order: the moment
/// its story begins, such as when its ink or action began or its cue was spoken.
///
/// Each keyframe's text runs from the start of the clause in progress at its anchor until the
/// next keyframe's text starts, so the narration is partitioned in order with nothing repeated or
/// dropped. Narration before the first keyframe belongs to it. Each text is trimmed, and a
/// keyframe with nothing said gets `None`.
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
fn clauses(words: &[&Word]) -> Vec<Span> {
    let mut clauses: Vec<Span> = Vec::new();
    let mut clause_words = 0;
    let mut previous: Option<&Word> = None;
    for word in words {
        let start = word.span.start();
        let continues = previous.is_some_and(|previous| {
            !ends_clause(&previous.text, clause_words) && start.since(previous.span.start()) < PAUSE
        });
        let reach = Span::new(start, word.span.end().min(start.saturating_add(WORD_REACH)));
        match clauses.last_mut() {
            Some(clause) if continues => {
                *clause = clause.union(reach);
                clause_words = clause_words.saturating_add(usize::from(word.is_spoken()));
            },
            _ => {
                clauses.push(reach);
                clause_words = usize::from(word.is_spoken());
            },
        }
        previous = Some(word);
    }
    clauses
}

/// Where each keyframe's text starts: the start of the clause holding its anchor. When two
/// keyframes fall in one clause, the later one takes the words from its own moment.
fn text_starts(anchors: &[Moment], clauses: &[Span]) -> Vec<Moment> {
    let mut starts: Vec<Moment> = Vec::with_capacity(anchors.len());
    for &anchor in anchors {
        let mut start = clauses
            .iter()
            .find(|clause| clause.contains(anchor))
            .map_or(anchor, |clause| clause.start());
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
mod tests;
