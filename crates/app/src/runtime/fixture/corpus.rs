use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{Report, paired, sha256};

pub(super) const WORD_SCORING: &str = "lowercase-alphanumeric-runs-v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Corpus {
    pub clips: Vec<Case>,
    #[serde(skip)]
    pub sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub id: String,
    pub tags: BTreeSet<String>,
    wav: PathBuf,
    reference: PathBuf,
    wav_sha256: String,
    reference_sha256: String,
}

pub(super) struct Clip {
    pub wav: Vec<u8>,
    pub reference: String,
}

impl Clip {
    pub(super) fn read(wav: &Path, reference: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            wav: std::fs::read(wav)?,
            reference: std::fs::read_to_string(reference)?,
        })
    }
}

impl Corpus {
    pub(super) fn read(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let mut corpus: Self = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            (1..=256).contains(&corpus.clips.len()),
            "A corpus needs 1 to 256 clips"
        );
        let mut ids = BTreeSet::new();
        for case in &corpus.clips {
            let label = |value: &str| {
                !value.is_empty()
                    && value.len() <= 80
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
            };
            anyhow::ensure!(
                label(&case.id) && case.tags.iter().all(|tag| label(tag)),
                "Corpus IDs and tags need short ASCII labels"
            );
            anyhow::ensure!(ids.insert(&case.id), "Corpus clip IDs must be unique");
        }
        corpus.sha256 = sha256(&bytes);
        Ok(corpus)
    }

    pub(super) fn summarize(&self, reports: &[Report]) -> Summary {
        let mut groups = BTreeMap::<&str, Vec<&Report>>::new();
        for (case, report) in self.clips.iter().zip(reports) {
            for tag in &case.tags {
                groups.entry(tag).or_default().push(report);
            }
        }
        if reports
            .first()
            .is_some_and(|report| matches!(report.evaluation, paired::Evaluation::Raw { .. }))
        {
            Summary::Raw {
                cleanup: "raw-replay",
                overall: paired::Metrics::new(reports.iter()),
                by_tag: groups
                    .into_iter()
                    .map(|(tag, rows)| (tag.to_owned(), paired::Metrics::new(rows.into_iter())))
                    .collect(),
            }
        } else {
            Summary::Production {
                overall: Metrics::new(reports.iter()),
                by_tag: groups
                    .into_iter()
                    .map(|(tag, rows)| (tag.to_owned(), Metrics::new(rows.into_iter())))
                    .collect(),
            }
        }
    }
}

impl Case {
    pub(super) fn load(&self, root: &Path) -> anyhow::Result<Clip> {
        let clip = Clip::read(&root.join(&self.wav), &root.join(&self.reference))?;
        anyhow::ensure!(
            sha256(&clip.wav) == self.wav_sha256,
            "Corpus audio hash mismatch for {}",
            self.id
        );
        anyhow::ensure!(
            sha256(clip.reference.as_bytes()) == self.reference_sha256,
            "Corpus reference hash mismatch for {}",
            self.id
        );
        Ok(clip)
    }
}

#[derive(Serialize)]
pub(super) struct Score {
    word_scoring: &'static str,
    pub reference_words: usize,
    pub heard_words: usize,
    pub word_errors: usize,
    pub formatting_reference_words: usize,
    pub formatting_word_errors: usize,
    text_sha256: String,
}

impl Score {
    pub(super) fn new(reference: &str, text: &str) -> Self {
        let expected = words(reference);
        let heard = words(text);
        let formatted: Vec<_> = reference.split_whitespace().collect();
        Self {
            word_scoring: WORD_SCORING,
            reference_words: expected.len(),
            heard_words: heard.len(),
            word_errors: distance(&expected, &heard),
            formatting_reference_words: formatted.len(),
            formatting_word_errors: distance(
                &formatted,
                &text.split_whitespace().collect::<Vec<_>>(),
            ),
            text_sha256: sha256(text.as_bytes()),
        }
    }
}

pub(super) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum Summary {
    Production {
        overall: Metrics,
        by_tag: BTreeMap<String, Metrics>,
    },
    Raw {
        cleanup: &'static str,
        overall: paired::Metrics,
        by_tag: BTreeMap<String, paired::Metrics>,
    },
}

#[derive(Serialize)]
pub(super) struct Metrics {
    clips: usize,
    inserted_clips: usize,
    #[serde(flatten)]
    quality: Quality,
    realtime_insertions: usize,
    stop_to_fake_insert_p50_ms: Option<f64>,
    stop_to_fake_insert_p95_ms: Option<f64>,
}

impl Metrics {
    fn new<'a>(reports: impl Iterator<Item = &'a Report>) -> Self {
        let reports: Vec<_> = reports.collect();
        let mut latency: Vec<_> = reports
            .iter()
            .filter(|r| r.speed == 1)
            .filter_map(|r| r.stop_to_fake_insert_ms)
            .collect();
        latency.sort_by(f64::total_cmp);
        let percentile = |percent: usize| {
            latency
                .get((latency.len().saturating_mul(percent).div_ceil(100)).saturating_sub(1))
                .copied()
        };
        Self {
            clips: reports.len(),
            inserted_clips: reports
                .iter()
                .filter(|r| r.stop_to_fake_insert_ms.is_some())
                .count(),
            quality: Quality::new(reports.iter().map(|r| r.evaluation.score())),
            realtime_insertions: latency.len(),
            stop_to_fake_insert_p50_ms: percentile(50),
            stop_to_fake_insert_p95_ms: percentile(95),
        }
    }
}

#[derive(Serialize)]
pub(super) struct Quality {
    reference_words: usize,
    word_errors: usize,
    word_error_rate: Option<f64>,
    formatting_reference_words: usize,
    formatting_word_errors: usize,
    formatting_word_error_rate: Option<f64>,
}

impl Quality {
    pub(super) fn new<'a>(scores: impl Iterator<Item = &'a Score>) -> Self {
        let scores: Vec<_> = scores.collect();
        let reference_words = scores
            .iter()
            .map(|score| score.reference_words)
            .sum::<usize>();
        let word_errors = scores.iter().map(|score| score.word_errors).sum::<usize>();
        let formatting_reference_words = scores
            .iter()
            .map(|score| score.formatting_reference_words)
            .sum::<usize>();
        let formatting_word_errors = scores
            .iter()
            .map(|score| score.formatting_word_errors)
            .sum::<usize>();
        Self {
            reference_words,
            word_errors,
            word_error_rate: (reference_words > 0)
                .then(|| word_errors as f64 / reference_words as f64),
            formatting_reference_words,
            formatting_word_errors,
            formatting_word_error_rate: (formatting_reference_words > 0)
                .then(|| formatting_word_errors as f64 / formatting_reference_words as f64),
        }
    }
}

fn distance<T: PartialEq>(expected: &[T], heard: &[T]) -> usize {
    let mut row: Vec<_> = (0..=heard.len()).collect();
    for (i, word) in expected.iter().enumerate() {
        let mut previous = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i.saturating_add(1);
        }
        let mut left = i.saturating_add(1);
        for (cell, actual) in row.iter_mut().skip(1).zip(heard) {
            let above = *cell;
            *cell = above
                .saturating_add(1)
                .min(left.saturating_add(1))
                .min(previous.saturating_add(usize::from(word != actual)));
            previous = above;
            left = *cell;
        }
    }
    row.last().copied().unwrap_or(expected.len())
}

mod tests;
