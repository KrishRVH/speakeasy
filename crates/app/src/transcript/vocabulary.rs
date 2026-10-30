//! Explicit local phrase corrections. Matching is whole-token, longest-first, and never chained;
//! replacements preserve edge punctuation and the full timing span of the words they replace.

use std::{collections::HashSet, fmt};

use anyhow::ensure;
use serde::{Deserialize, Serialize};
use speakeasy_show::{Narration, Word};

pub(crate) const LIMIT: usize = 200;
const PHRASE_LIMIT: usize = 64;
const WORD_LIMIT: usize = 6;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Correction {
    pub heard: String,
    pub written: String,
}

impl fmt::Debug for Correction {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Correction").finish_non_exhaustive()
    }
}

pub(crate) fn validate(rules: &[Correction]) -> anyhow::Result<()> {
    ensure!(
        rules.len() <= LIMIT,
        "Keep at most {LIMIT} phrase corrections."
    );
    let mut seen = HashSet::new();
    for (index, rule) in rules.iter().enumerate() {
        let row = index.saturating_add(1);
        for value in [&rule.heard, &rule.written] {
            ensure!(
                !value.trim().is_empty()
                    && !value.chars().any(char::is_control)
                    && value.chars().count() <= PHRASE_LIMIT
                    && value.split_whitespace().count() <= WORD_LIMIT,
                "Correction {row} needs nonempty phrases of at most {PHRASE_LIMIT} characters and {WORD_LIMIT} words, without control characters."
            );
        }
        ensure!(
            rule.heard
                .split_whitespace()
                .all(|word| core(word) == word && !word.is_empty()),
            "Correction {row}: omit surrounding punctuation from the phrase you say."
        );
        let heard: Vec<_> = rule
            .heard
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        ensure!(
            seen.insert(heard),
            "Correction {row} repeats an earlier spoken phrase."
        );
    }
    Ok(())
}

/// Compiled only for a transcription request, never retained alongside engine text or logs.
pub(crate) struct Vocabulary<'a> {
    rules: Vec<(Vec<String>, &'a str)>,
}

impl<'a> Vocabulary<'a> {
    pub(crate) fn new(rules: &'a [Correction]) -> Self {
        let mut rules: Vec<_> = rules
            .iter()
            .map(|rule| {
                (
                    rule.heard
                        .split_whitespace()
                        .map(str::to_lowercase)
                        .collect::<Vec<_>>(),
                    rule.written.trim(),
                )
            })
            .collect();
        rules.sort_by_key(|(heard, _)| std::cmp::Reverse(heard.len()));
        Self { rules }
    }

    pub(crate) fn text(&self, text: &str) -> String {
        if self.rules.is_empty() {
            return text.to_owned();
        }
        let tokens: Vec<_> = text.split_whitespace().collect();
        let mut output = Vec::new();
        let mut rest = tokens.as_slice();
        while let Some((first, tail)) = rest.split_first() {
            if let Some((count, written)) = self.replacement(rest.iter().copied()) {
                output.push(written);
                rest = rest.get(count..).unwrap_or_default();
            } else {
                output.push((*first).to_owned());
                rest = tail;
            }
        }
        output.join(" ")
    }

    pub(crate) fn narration(&self, narration: Narration) -> Narration {
        match narration {
            Narration::Text(text) => Narration::Text(self.text(&text)),
            Narration::Words(words) if !self.rules.is_empty() => {
                let mut output = Vec::with_capacity(words.len());
                let mut rest = words.as_slice();
                while let Some((first, tail)) = rest.split_first() {
                    if let Some((count, text)) =
                        self.replacement(rest.iter().map(|word| word.text.as_str()))
                    {
                        let replaced = rest.get(..count).unwrap_or_default();
                        let span = replaced
                            .iter()
                            .fold(first.span, |span, word| span.union(word.span));
                        output.push(Word { text, span });
                        rest = rest.get(count..).unwrap_or_default();
                    } else {
                        output.push(first.clone());
                        rest = tail;
                    }
                }
                Narration::Words(output)
            },
            narration @ Narration::Words(_) => narration,
        }
    }

    fn replacement<'b>(
        &self,
        tokens: impl Iterator<Item = &'b str> + Clone,
    ) -> Option<(usize, String)> {
        for (heard, written) in &self.rules {
            let mut words = tokens.clone();
            let mut prefix = "";
            let mut suffix = "";
            let matches = heard.iter().enumerate().all(|(index, expected)| {
                let Some(word) = words.next() else {
                    return false;
                };
                let spoken = core(word);
                if spoken.to_lowercase() != *expected {
                    return false;
                }
                let leading = word
                    .len()
                    .saturating_sub(word.trim_start_matches(punctuation).len());
                let trailing = leading.saturating_add(spoken.len());
                let before = word.get(..leading).unwrap_or_default();
                let after = word.get(trailing..).unwrap_or_default();
                if index == 0 {
                    prefix = before;
                } else if !before.is_empty() {
                    return false;
                }
                if index.saturating_add(1) == heard.len() {
                    suffix = after;
                } else if !after.is_empty() {
                    return false;
                }
                true
            });
            if matches && !heard.is_empty() {
                return Some((heard.len(), format!("{prefix}{written}{suffix}")));
            }
        }
        None
    }
}

fn core(word: &str) -> &str {
    word.trim_matches(punctuation)
}

fn punctuation(ch: char) -> bool {
    matches!(
        ch,
        '.' | ','
            | '!'
            | '?'
            | ';'
            | ':'
            | '…'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '"'
            | '\''
            | '“'
            | '”'
            | '‘'
            | '’'
    )
}

#[cfg(test)]
mod tests;
