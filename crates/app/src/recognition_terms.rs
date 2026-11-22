use std::collections::HashSet;

use anyhow::ensure;
use serde::{Deserialize, Serialize};

const LIMIT: usize = 100;
const PHRASE_LIMIT: usize = 64;
const WORD_LIMIT: usize = 6;
const PROMPT_CHARACTERS: usize = 600;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Terms(Vec<String>);

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
        let mut room = PROMPT_CHARACTERS;
        let mut newest: Vec<&str> = self
            .0
            .iter()
            .rev()
            .take_while(|phrase| {
                let cost = phrase.chars().count().saturating_add(2);
                let fits = cost <= room;
                room = room.saturating_sub(cost);
                fits
            })
            .map(String::as_str)
            .collect();
        newest.reverse();
        newest.join(", ")
    }

    pub(crate) fn list_echo(&self) -> Option<String> {
        (self.0.len() > 1).then(|| self.prompt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whisper_prompt_keeps_the_newest_hints_that_fit() -> anyhow::Result<()> {
        let mut terms = Terms::default();
        for index in 0..LIMIT {
            terms.add(&format!("Hint{index:02} {}", "x".repeat(50)))?;
        }
        let prompt = terms.prompt();
        assert!(prompt.chars().count() <= PROMPT_CHARACTERS);
        assert!(prompt.ends_with(terms.phrases().last().map_or("", String::as_str)));
        assert!(!prompt.contains("Hint00"));
        Ok(())
    }

    #[test]
    fn only_a_list_of_hints_can_be_echoed() -> anyhow::Result<()> {
        let mut terms = Terms::default();
        terms.add("Kubernetes")?;
        assert_eq!(
            terms.list_echo(),
            None,
            "Saying the one hint would be dropped"
        );
        terms.add("Example Name")?;
        assert_eq!(
            terms.list_echo().as_deref(),
            Some("Kubernetes, Example Name")
        );
        Ok(())
    }

    #[test]
    fn invalid_additions_preserve_the_hints() -> anyhow::Result<()> {
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
            assert!(terms.add(&phrase).is_err());
            assert_eq!(terms, before);
        }
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
