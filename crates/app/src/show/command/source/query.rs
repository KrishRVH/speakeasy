//! Private query inputs and content-free location results shared by source commands.

use anyhow::Context as _;
use serde::Serialize;
use speakeasy_show::{Manifest, Moment, Span, locate::Focus};

#[derive(PartialEq, Eq)]
pub(super) enum Query {
    At(Moment),
    Mark(usize),
    Phrase(String),
    Match { phrase: String, occurrence: usize },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Found {
    pub kind: &'static str,
    pub number: Option<usize>,
    #[serde(rename = "match")]
    pub occurrence: Option<usize>,
    pub matches: Vec<Span>,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    #[serde(skip)]
    pub selected: Option<Span>,
}

impl Query {
    pub(super) fn resolve(&self, manifest: &Manifest) -> anyhow::Result<Found> {
        let (focus, kind, number, occurrence) = match self {
            Self::At(at) => (Focus::At(*at), "time", None, None),
            Self::Mark(number) => (Focus::Mark(*number), "mark", Some(*number), None),
            Self::Phrase(phrase) => (Focus::Phrase(phrase), "phrase", None, None),
            Self::Match { phrase, occurrence } => {
                (Focus::Phrase(phrase), "phrase", None, Some(*occurrence))
            },
        };
        let matches = manifest.locate(focus)?;
        anyhow::ensure!(
            !matches.is_empty(),
            "No matching phrase in the recorded narration; use another phrase, mark, or time"
        );
        let selected = match occurrence {
            Some(index) => Some(
                *index
                    .checked_sub(1)
                    .and_then(|index| matches.get(index))
                    .context("That match number is unavailable; list the phrase matches first")?,
            ),
            None if matches.len() == 1 => matches.first().copied(),
            None => None,
        };
        Ok(Found {
            kind,
            number,
            occurrence: occurrence
                .or_else(|| (kind == "phrase" && selected.is_some()).then_some(1)),
            matches,
            start_ms: selected.map(|span| span.start().as_millis()),
            end_ms: selected.map(|span| span.end().as_millis()),
            selected,
        })
    }
}

pub(super) fn time(value: &str) -> anyhow::Result<Moment> {
    let number = |part: &str| -> anyhow::Result<u64> {
        anyhow::ensure!(
            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()),
            "Time must be seconds or M:SS, with up to three decimal places"
        );
        Ok(part.parse()?)
    };
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    anyhow::ensure!(
        fraction.len() <= 3 && (!value.contains('.') || !fraction.is_empty()),
        "Time accepts one to three decimal places"
    );
    let seconds = if let Some((minutes, seconds)) = whole.split_once(':') {
        let seconds = number(seconds)?;
        anyhow::ensure!(seconds < 60, "Seconds after ':' must be less than 60");
        number(minutes)?
            .checked_mul(60)
            .and_then(|minutes| minutes.checked_add(seconds))
    } else {
        Some(number(whole)?)
    }
    .context("Time is too large")?;
    let millis = if fraction.is_empty() {
        0
    } else {
        number(fraction)?
            .saturating_mul(10_u64.pow(3_u32.saturating_sub(u32::try_from(fraction.len())?)))
    };
    Ok(Moment::from_millis(
        seconds
            .checked_mul(1_000)
            .and_then(|seconds| seconds.checked_add(millis))
            .context("Time is too large")?,
    ))
}
