use std::{cmp::Ordering, collections::BTreeMap, ffi::OsStr, path::Path};

use serde::Serialize;

use super::{
    Config, Report,
    corpus::{self, Quality, Score},
};
use crate::transcript::{self, style::Style};

pub(super) enum Cleanup {
    Production,
    Raw { candidate: Option<Box<Config>> },
}

impl Cleanup {
    pub(super) fn read(
        mode: Option<&OsStr>,
        candidate: Option<&Path>,
        current: &Config,
    ) -> anyhow::Result<Self> {
        match mode.unwrap_or_else(|| OsStr::new("production")).to_str() {
            Some("production") => {
                anyhow::ensure!(
                    candidate.is_none(),
                    "A candidate requires raw cleanup replay"
                );
                Ok(Self::Production)
            },
            Some("raw") => {
                let candidate = candidate
                    .map(|path| {
                        let config = Config::read(path)?.validated(path)?.into_inner();
                        validate_candidate(current, &config)?;
                        Ok::<_, anyhow::Error>(Box::new(config))
                    })
                    .transpose()?;
                Ok(Self::Raw { candidate })
            },
            _ => anyhow::bail!("Fixture cleanup must be production or raw"),
        }
    }

    pub(super) fn prepare(&self, config: &Config) -> (Config, Style) {
        let mut config = config.clone();
        let style = match self {
            Self::Production => Style::Prose,
            Self::Raw { .. } => {
                config.remove_fillers = false;
                config.vocabulary.clear();
                Style::Verbatim
            },
        };
        (config, style)
    }

    pub(super) fn evaluate(&self, reference: &str, text: &str, current: &Config) -> Evaluation {
        match self {
            Self::Production => Evaluation::Production(Score::new(reference, text)),
            Self::Raw { candidate } => {
                let raw = Score::new(reference, text);
                let mut policies = vec![Policy::new("current", reference, text, &raw, current)];
                if let Some(candidate) = candidate {
                    let mut scored = Policy::new("candidate", reference, text, &raw, candidate);
                    scored.versus_current = policies
                        .first()
                        .map(|current| Outcome::new(&scored.score, &current.score));
                    policies.push(scored);
                }
                Evaluation::Raw {
                    cleanup: "raw-replay",
                    raw,
                    policies,
                }
            },
        }
    }
}

fn validate_candidate(current: &Config, candidate: &Config) -> anyhow::Result<()> {
    let mut normalized = candidate.clone();
    normalized.vocabulary.clone_from(&current.vocabulary);
    normalized.remove_fillers = current.remove_fillers;
    anyhow::ensure!(
        normalized == *current,
        "The candidate may change only corrections and filler removal"
    );
    Ok(())
}

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum Evaluation {
    Production(Score),
    Raw {
        cleanup: &'static str,
        raw: Score,
        policies: Vec<Policy>,
    },
}

impl Evaluation {
    pub(super) const fn score(&self) -> &Score {
        match self {
            Self::Production(score) | Self::Raw { raw: score, .. } => score,
        }
    }
}

#[derive(Serialize)]
pub(super) struct Policy {
    name: &'static str,
    score: Score,
    versus_raw: Damage,
    #[serde(skip_serializing_if = "Option::is_none")]
    versus_current: Option<Outcome>,
}

impl Policy {
    fn new(
        name: &'static str,
        reference: &str,
        raw: &str,
        raw_score: &Score,
        config: &Config,
    ) -> Self {
        let text = transcript::for_dictation(
            raw,
            Style::Prose,
            config.fillers(&config.language),
            &config.vocabulary,
        );
        let score = Score::new(reference, &text);
        Self {
            name,
            versus_raw: Damage::new(reference, raw, &text, Outcome::new(&score, raw_score)),
            score,
            versus_current: None,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
struct Outcome {
    words: Change,
    formatting: Change,
}

impl Outcome {
    fn new(score: &Score, baseline: &Score) -> Self {
        Self {
            words: Change::new(score.word_errors, baseline.word_errors),
            formatting: Change::new(
                score.formatting_word_errors,
                baseline.formatting_word_errors,
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Change {
    Win,
    Loss,
    Tie,
}

impl Change {
    fn new(errors: usize, baseline: usize) -> Self {
        match errors.cmp(&baseline) {
            Ordering::Less => Self::Win,
            Ordering::Greater => Self::Loss,
            Ordering::Equal => Self::Tie,
        }
    }
}

#[derive(Serialize)]
struct Damage {
    outcome: Outcome,
    changed: bool,
    introduced_tokens: usize,
    removed_reference_words: usize,
    digit_tokens_lost: usize,
}

impl Damage {
    fn new(reference: &str, raw: &str, text: &str, outcome: Outcome) -> Self {
        let counts = |text| {
            let mut counts = BTreeMap::<String, usize>::new();
            for token in corpus::words(text) {
                let count = counts.entry(token).or_default();
                *count = count.saturating_add(1);
            }
            counts
        };
        let expected = counts(reference);
        let heard = counts(raw);
        let cleaned = counts(text);
        Self {
            outcome,
            changed: raw != text,
            introduced_tokens: cleaned
                .iter()
                .filter(|(word, _)| !heard.contains_key(*word) && !expected.contains_key(*word))
                .map(|(_, count)| count)
                .sum(),
            removed_reference_words: heard
                .iter()
                .map(|(word, &count)| {
                    count
                        .min(expected.get(word).copied().unwrap_or(0))
                        .saturating_sub(cleaned.get(word).copied().unwrap_or(0))
                })
                .sum(),
            digit_tokens_lost: heard
                .iter()
                .filter(|(word, _)| word.chars().any(char::is_numeric))
                .map(|(word, &count)| count.saturating_sub(cleaned.get(word).copied().unwrap_or(0)))
                .sum(),
        }
    }
}

#[derive(Default, Serialize)]
struct Changes {
    wins: usize,
    losses: usize,
    ties: usize,
}

impl Changes {
    fn new(changes: impl Iterator<Item = Change>) -> Self {
        let mut counts = Self::default();
        for change in changes {
            match change {
                Change::Win => counts.wins = counts.wins.saturating_add(1),
                Change::Loss => counts.losses = counts.losses.saturating_add(1),
                Change::Tie => counts.ties = counts.ties.saturating_add(1),
            }
        }
        counts
    }
}

#[derive(Serialize)]
struct Outcomes {
    words: Changes,
    formatting: Changes,
}

impl Outcomes {
    fn new(outcomes: impl Iterator<Item = Outcome> + Clone) -> Self {
        Self {
            words: Changes::new(outcomes.clone().map(|outcome| outcome.words)),
            formatting: Changes::new(outcomes.map(|outcome| outcome.formatting)),
        }
    }
}

#[derive(Serialize)]
struct PolicyMetrics {
    name: &'static str,
    #[serde(flatten)]
    quality: Quality,
    versus_raw: Outcomes,
    #[serde(skip_serializing_if = "Option::is_none")]
    versus_current: Option<Outcomes>,
    changed_clips: usize,
    damaged_clips: usize,
    introduced_tokens: usize,
    removed_reference_words: usize,
    digit_tokens_lost: usize,
}

#[derive(Serialize)]
pub(super) struct Metrics {
    clips: usize,
    raw_inserted_clips: usize,
    raw: Quality,
    policies: Vec<PolicyMetrics>,
}

impl Metrics {
    pub(super) fn new<'a>(reports: impl Iterator<Item = &'a Report>) -> Self {
        let reports: Vec<_> = reports.collect();
        let mut grouped = BTreeMap::<&str, Vec<&Policy>>::new();
        for report in &reports {
            if let Evaluation::Raw { policies, .. } = &report.evaluation {
                for policy in policies {
                    grouped.entry(policy.name).or_default().push(policy);
                }
            }
        }
        Self {
            clips: reports.len(),
            raw_inserted_clips: reports
                .iter()
                .filter(|report| report.stop_to_fake_insert_ms.is_some())
                .count(),
            raw: Quality::new(reports.iter().map(|report| report.evaluation.score())),
            policies: grouped
                .into_values()
                .filter_map(|policies| {
                    let name = policies.first()?.name;
                    Some(PolicyMetrics {
                        name,
                        quality: Quality::new(policies.iter().map(|policy| &policy.score)),
                        versus_raw: Outcomes::new(
                            policies.iter().map(|policy| policy.versus_raw.outcome),
                        ),
                        versus_current: policies
                            .iter()
                            .any(|policy| policy.versus_current.is_some())
                            .then(|| {
                                Outcomes::new(
                                    policies.iter().filter_map(|policy| policy.versus_current),
                                )
                            }),
                        changed_clips: policies
                            .iter()
                            .filter(|policy| policy.versus_raw.changed)
                            .count(),
                        damaged_clips: policies
                            .iter()
                            .filter(|policy| policy.versus_raw.outcome.words == Change::Loss)
                            .count(),
                        introduced_tokens: policies
                            .iter()
                            .map(|policy| policy.versus_raw.introduced_tokens)
                            .sum(),
                        removed_reference_words: policies
                            .iter()
                            .map(|policy| policy.versus_raw.removed_reference_words)
                            .sum(),
                        digit_tokens_lost: policies
                            .iter()
                            .map(|policy| policy.versus_raw.digit_tokens_lost)
                            .sum(),
                    })
                })
                .collect(),
        }
    }
}

mod tests;
