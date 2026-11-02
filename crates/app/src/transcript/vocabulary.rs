use std::{cmp::Reverse, collections::HashSet, fmt};

use anyhow::ensure;
use serde::{Deserialize, Serialize};
use speakeasy_show::{Narration, Word};

const LIMIT: usize = 200;
const PHRASE_LIMIT: usize = 64;
const WORD_LIMIT: usize = 6;
const PUNCTUATION: [char; 19] = [
    '.', ',', '!', '?', ';', ':', '…', '(', ')', '[', ']', '{', '}', '"', '\'', '“', '”', '‘', '’',
];

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
                .all(|word| word.trim_matches(PUNCTUATION) == word),
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

pub(crate) struct Vocabulary<'a> {
    rules: Vec<(Vec<String>, &'a str)>,
}

impl<'a> Vocabulary<'a> {
    pub(crate) fn new(rules: &'a [Correction]) -> Self {
        let mut rules: Vec<_> = rules
            .iter()
            .map(|rule| {
                let heard: Vec<_> = rule
                    .heard
                    .split_whitespace()
                    .map(str::to_lowercase)
                    .collect();
                (heard, rule.written.trim())
            })
            .filter(|(heard, _)| !heard.is_empty())
            .collect();
        rules.sort_by_key(|(heard, _)| Reverse(heard.len()));
        Self { rules }
    }

    pub(crate) fn text(&self, text: &str) -> String {
        if self.rules.is_empty() {
            return text.to_owned();
        }
        let tokens: Vec<_> = text.split_whitespace().collect();
        let mut output = String::with_capacity(text.len());
        let mut rest = tokens.as_slice();
        while let Some((first, tail)) = rest.split_first() {
            if !output.is_empty() {
                output.push(' ');
            }
            if let Some((count, written)) = self.replacement(rest.iter().copied()) {
                output.push_str(&written);
                rest = rest.get(count..).unwrap_or_default();
            } else {
                output.push_str(first);
                rest = tail;
            }
        }
        output
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
        self.rules.iter().find_map(|(heard, written)| {
            let last = heard.len().saturating_sub(1);
            let mut words = tokens.clone();
            let (mut prefix, mut suffix) = ("", "");
            for (index, expected) in heard.iter().enumerate() {
                let word = words.next()?;
                let rest = word.trim_start_matches(PUNCTUATION);
                let spoken = rest.trim_end_matches(PUNCTUATION);
                let before = word.strip_suffix(rest)?;
                let after = rest.strip_prefix(spoken)?;
                if spoken.to_lowercase() != *expected
                    || (index > 0 && !before.is_empty())
                    || (index < last && !after.is_empty())
                {
                    return None;
                }
                if index == 0 {
                    prefix = before;
                }
                if index == last {
                    suffix = after;
                }
            }
            Some((heard.len(), format!("{prefix}{written}{suffix}")))
        })
    }
}

#[cfg(test)]
mod tests;
