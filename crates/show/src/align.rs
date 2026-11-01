use std::{fmt, slice, time::Duration};

use crate::{
    Moment, Span,
    recording::{Narration, Word},
};

const PAUSE: Duration = Duration::from_secs(2);

const WORD_REACH: Duration = Duration::from_millis(700);

const HESITATION: Duration = Duration::from_secs(4);

const SPEECH_LAG: Duration = Duration::from_secs(1);

const ONSET_TOLERANCE: Duration = Duration::from_millis(50);

const COMMA_CLAUSE_WORDS: usize = 3;

#[derive(Clone, PartialEq, Eq)]
pub struct TimedClause {
    pub at: Moment,
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

pub(crate) fn said_before(anchor: Moment, narration: &Narration) -> bool {
    let words = ordered(narration.words());
    let starts = text_starts(&[anchor], &clauses(&words));
    starts.first().is_some_and(|start| {
        words
            .first()
            .is_some_and(|word| word.span.start().saturating_add(ONSET_TOLERANCE) < *start)
    })
}

struct Clause {
    first: usize,
    span: Span,
    resumes: bool,
}

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

fn text_starts(anchors: &[Moment], clauses: &[Clause]) -> Vec<Moment> {
    let mut starts: Vec<Moment> = Vec::with_capacity(anchors.len());
    let mut anchors = anchors.iter().copied().peekable();
    while let Some(anchor) = anchors.next() {
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

fn in_progress(clauses: &[Clause], moment: Moment) -> Option<&[Clause]> {
    clauses
        .chunk_by(|_, clause| clause.resumes)
        .find_map(|run| {
            let current = run.iter().position(|clause| clause.span.contains(moment))?;
            run.get(..=current)
        })
}

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

fn attaches(word: &str) -> bool {
    word.starts_with([',', '.', '!', '?', ';', ':', '’', '\'', ')', ']', '”'])
        && !word.chars().any(char::is_alphanumeric)
}

fn append(text: &mut String, word: &str) {
    if !text.is_empty() && !attaches(word) && !text.ends_with(['(', '[', '“']) {
        text.push(' ');
    }
    text.push_str(word);
}

#[cfg(test)]
mod tests;
