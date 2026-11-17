use std::{collections::HashSet, fmt};

use anyhow::ensure;
use serde::{Deserialize, Serialize};

const LIMIT: usize = 100;
const PHRASE_LIMIT: usize = 64;
const WORD_LIMIT: usize = 6;

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Terms(Vec<String>);

impl fmt::Debug for Terms {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Terms")
            .field("count", &self.0.len())
            .finish()
    }
}

impl Terms {
    pub(crate) fn phrases(&self) -> &[String] {
        &self.0
    }

    pub(crate) fn add(&mut self, phrase: &str) -> anyhow::Result<()> {
        self.0.push(phrase.trim().to_owned());
        self.validate().inspect_err(|_| {
            self.0.pop();
        })
    }

    pub(crate) fn remove(&mut self, index: usize) -> bool {
        if index < self.0.len() {
            self.0.remove(index);
            true
        } else {
            false
        }
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.0.len() <= LIMIT,
            "Keep recognition hints to {LIMIT} names or phrases."
        );
        let mut seen = HashSet::new();
        for phrase in &self.0 {
            ensure!(
                !phrase.is_empty()
                    && phrase == phrase.trim()
                    && phrase.chars().count() <= PHRASE_LIMIT
                    && phrase.split_whitespace().count() <= WORD_LIMIT
                    && !phrase.chars().any(char::is_control),
                "Each recognition hint needs 1–{PHRASE_LIMIT} characters and at most {WORD_LIMIT} words, without surrounding whitespace or control characters."
            );
            ensure!(
                seen.insert(phrase.to_lowercase()),
                "This recognition hint is already listed."
            );
        }
        Ok(())
    }

    pub(crate) fn contexts(&self) -> serde_json::Value {
        serde_json::json!([{"phrases": self.0, "boost": 2.0}])
    }

    pub(crate) fn prompt(&self) -> String {
        self.0.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_additions_preserve_the_vocabulary_and_errors_are_private() -> anyhow::Result<()> {
        let mut terms = Terms::default();
        terms.add("Private project")?;
        for phrase in [
            "PRIVATE PROJECT".into(),
            String::new(),
            "x".repeat(65),
            "private\ncontent".into(),
            "a b c d e f g".into(),
        ] {
            let before = terms.clone();
            let error = terms.add(&phrase).unwrap_err().to_string();
            assert!(!error.contains("PRIVATE PROJECT"));
            assert_eq!(terms, before);
        }
        assert!(!format!("{terms:?}").contains("Private"));
        Ok(())
    }

    #[test]
    fn removal_reports_whether_the_hints_changed() -> anyhow::Result<()> {
        let mut terms = Terms::default();
        terms.add("Kubernetes")?;
        assert!(!terms.remove(1));
        assert_eq!(terms.phrases(), ["Kubernetes"]);
        assert!(terms.remove(0));
        assert!(!terms.remove(0));
        assert_eq!(terms, Terms::default());
        Ok(())
    }
}
