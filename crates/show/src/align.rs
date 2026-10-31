//! Dividing the narration among keyframes.

use std::{fmt, slice, time::Duration};

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
/// A clause left unfinished stays in progress through the silence after it until speech resumes
/// or, at the latest, this long past its last word's start, and a clause beginning by then
/// resumes it: people stop mid-sentence to draw what they mean, then go on.
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

/// A readable group of timed words, starting at its first word's reported onset. It makes no
/// claim about how long the words or a nearby frame remained in progress.
#[derive(Clone, PartialEq, Eq)]
pub struct TimedClause {
    /// The first word's reported start.
    pub at: Moment,
    /// Recognized words with their punctuation, never summary edits.
    pub text: String,
}

impl fmt::Debug for TimedClause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimedClause")
            .field("at", &self.at)
            .field("bytes", &self.text.len())
            .finish()
    }
}

/// Groups words using the keyframe alignment's clause boundaries, in stable onset order.
///
/// Empty words are omitted and punctuation stays with its word. A spoken word starting ten seconds or
/// more after the row began starts another row, without changing keyframe alignment.
#[must_use]
pub fn timed_clauses(words: &[Word]) -> Vec<TimedClause> {
    let words = ordered(words);
    let clauses = clauses(&words);
    let mut boundaries = clauses.iter().map(|clause| clause.first).peekable();
    let mut rows: Vec<TimedClause> = Vec::new();
    let mut boundary = false;
    for (index, word) in words.into_iter().enumerate() {
        boundary |= boundaries.next_if_eq(&index).is_some();
        let text = word.text.trim();
        let new_row = rows.last().is_none_or(|row| {
            !attaches(text)
                && (boundary || word.span.start().since(row.at) >= Duration::from_secs(10))
        });
        if !attaches(text) {
            boundary = false;
        }
        if new_row {
            rows.push(TimedClause {
                at: word.span.start(),
                text: String::new(),
            });
        }
        if let Some(row) = rows.last_mut() {
            append(&mut row.text, text);
        }
    }
    rows
}

fn ordered(words: &[Word]) -> Vec<&Word> {
    let mut words: Vec<_> = words
        .iter()
        .filter(|word| !word.text.trim().is_empty())
        .collect();
    words.sort_by_key(|word| word.span.start());
    words
}

/// The words said for each keyframe, given each keyframe's anchor in keyframe order: the moment
/// its story begins, such as when its ink or action began or its cue was spoken.
///
/// A keyframe's clause is the one in progress at its anchor or, for an anchor in the silence just
/// before speech, the clause about to begin, unless the next anchor lies nearer that clause's
/// start. A clause that resumes one left hanging carries it on, along with whatever that one
/// carries on. A keyframe's text starts at the first of these that follows the previous keyframe's
/// text start, or just after that start if none does: where each clause its clause carries on
/// began, earliest first, where its clause began, and its anchor. Each text runs until the next
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
            let words = ordered(words);
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
    first: usize,
    /// From the clause's first word until it is over.
    span: Span,
    /// Whether the clause resumes the one before it, which was left hanging.
    resumes: bool,
}

/// The clauses of `words`, in order.
fn clauses(words: &[&Word]) -> Vec<Clause> {
    let mut clauses: Vec<Clause> = Vec::new();
    let mut clause_words = 0;
    let mut previous: Option<&Word> = None;
    for (index, &word) in words.iter().enumerate() {
        let start = word.span.start();
        let hanging_since = previous
            .replace(word)
            .filter(|previous| !ends_clause(&previous.text, clause_words))
            .map(|previous| previous.span.start());
        let reach = Span::new(start, word.span.end().min(start.saturating_add(WORD_REACH)));
        let resumes = match (clauses.last_mut(), hanging_since) {
            (Some(clause), Some(last)) if start.since(last) < PAUSE => {
                clause.span = clause.span.union(reach);
                clause_words = clause_words.saturating_add(usize::from(word.is_spoken()));
                continue;
            },
            (Some(hanging), Some(last)) if start.since(last) <= HESITATION => {
                hanging.span = hanging.span.union(Span::at(start));
                true
            },
            (Some(hanging), Some(last)) => {
                hanging.span = hanging
                    .span
                    .union(Span::at(last.saturating_add(HESITATION)));
                false
            },
            _ => false,
        };
        clauses.push(Clause {
            first: index,
            span: reach,
            resumes,
        });
        clause_words = usize::from(word.is_spoken());
    }
    if let Some(last) = previous
        && !ends_clause(&last.text, clause_words)
        && let Some(hanging) = clauses.last_mut()
    {
        hanging.span = hanging
            .span
            .union(Span::at(last.span.start().saturating_add(HESITATION)));
    }
    clauses
}

/// Where each keyframe's text starts, by the rules [`align`] gives.
fn text_starts(anchors: &[Moment], clauses: &[Clause]) -> Vec<Moment> {
    let mut starts: Vec<Moment> = Vec::with_capacity(anchors.len());
    let mut anchors = anchors.iter().copied().peekable();
    while let Some(anchor) = anchors.next() {
        // A clause about to begin carries on none, since a clause left hanging lasts until the
        // one resuming it begins.
        let carried = in_progress(clauses, anchor)
            .or_else(|| {
                about_to_begin(clauses, anchor, anchors.peek().copied()).map(slice::from_ref)
            })
            .unwrap_or_default();
        let earliest = starts.last().map_or(Moment::START, |previous| {
            previous.saturating_add(Duration::from_millis(1))
        });
        let start = carried
            .iter()
            .map(|clause| clause.span.start())
            .chain([anchor])
            .find(|start| *start >= earliest)
            .unwrap_or(earliest);
        starts.push(start);
    }
    starts
}

/// The clause in progress at `moment`, after each clause it carries on, earliest first.
fn in_progress(clauses: &[Clause], moment: Moment) -> Option<&[Clause]> {
    clauses
        .chunk_by(|_, clause| clause.resumes)
        .find_map(|run| {
            let current = run.iter().position(|clause| clause.span.contains(moment))?;
            run.get(..=current)
        })
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

/// Whether `word` belongs to the word before it, as punctuation and closing marks do. A spoken word
/// never does, even one that opens with an apostrophe, such as "'cause".
fn attaches(word: &str) -> bool {
    word.starts_with([',', '.', '!', '?', ';', ':', '’', '\'', ')', ']', '”'])
        && !word.chars().any(char::is_alphanumeric)
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
