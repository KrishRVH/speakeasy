//! Private recognition hints, distinct from deterministic phrase corrections. Decoder limits keep
//! a large vocabulary from overwhelming ordinary speech; diagnostics never include its contents.

use std::{collections::HashSet, fmt};

use anyhow::ensure;
use serde::{Deserialize, Serialize};

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
        let mut next = self.clone();
        next.0.push(phrase.trim().to_owned());
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub(crate) fn remove(&mut self, index: usize) {
        if index < self.0.len() {
            self.0.remove(index);
        }
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.0.len() <= 100,
            "Keep recognition hints to 100 names or phrases."
        );
        let mut seen = HashSet::new();
        for phrase in &self.0 {
            ensure!(
                !phrase.trim().is_empty()
                    && phrase == phrase.trim()
                    && phrase.chars().count() <= 64
                    && phrase.split_whitespace().count() <= 6
                    && !phrase.chars().any(char::is_control),
                "Each recognition hint needs 1–64 characters and at most six words, without surrounding whitespace or control characters."
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
}
