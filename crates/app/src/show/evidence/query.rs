use anyhow::Context as _;
use serde::Serialize;
use speakeasy_show::{Manifest, Moment, Span, locate::Focus};

#[derive(PartialEq, Eq)]
pub(in crate::show) enum Query {
    At(Moment),
    Mark(usize),
    Phrase(String),
    Match { phrase: String, occurrence: usize },
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::show) enum Kind {
    Time,
    Mark,
    Phrase,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::show) struct Found {
    pub kind: Kind,
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
    pub(in crate::show) fn resolve(&self, manifest: &Manifest) -> anyhow::Result<Found> {
        let (focus, kind, number, occurrence) = match self {
            Self::At(at) => (Focus::At(*at), Kind::Time, None, None),
            Self::Mark(number) => (Focus::Mark(*number), Kind::Mark, Some(*number), None),
            Self::Phrase(phrase) => (Focus::Phrase(phrase), Kind::Phrase, None, None),
            Self::Match { phrase, occurrence } => {
                (Focus::Phrase(phrase), Kind::Phrase, None, Some(*occurrence))
            },
        };
        let matches = manifest.locate(focus)?;
        anyhow::ensure!(
            !matches.is_empty(),
            "No matching phrase in the recorded narration; use another phrase, mark, or time"
        );
        let selected = match (occurrence, matches.as_slice()) {
            (Some(nth), _) => Some(
                *nth.checked_sub(1)
                    .and_then(|index| matches.get(index))
                    .context("That match number is unavailable; list the phrase matches first")?,
            ),
            (None, &[only]) => Some(only),
            (None, _) => None,
        };
        Ok(Found {
            kind,
            number,
            occurrence: occurrence
                .or_else(|| (kind == Kind::Phrase && selected.is_some()).then_some(1)),
            matches,
            start_ms: selected.map(|span| span.start().as_millis()),
            end_ms: selected.map(|span| span.end().as_millis()),
            selected,
        })
    }
}

pub(in crate::show) fn time(value: &str) -> anyhow::Result<Moment> {
    let number = |part: &str| -> anyhow::Result<u64> {
        anyhow::ensure!(
            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()),
            "Time must be seconds or M:SS, with up to three decimal places"
        );
        Ok(part.parse()?)
    };
    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => {
            anyhow::ensure!(
                (1..=3).contains(&fraction.len()),
                "Time accepts one to three decimal places"
            );
            (whole, fraction)
        },
        None => (value, ""),
    };
    let seconds = match whole.split_once(':') {
        Some((minutes, seconds)) => {
            let seconds = number(seconds)?;
            anyhow::ensure!(seconds < 60, "Seconds after ':' must be less than 60");
            number(minutes)?
                .checked_mul(60)
                .and_then(|minutes| minutes.checked_add(seconds))
        },
        None => Some(number(whole)?),
    }
    .context("Time is too large")?;
    let millis = number(&format!("{fraction:0<3}"))?;
    Ok(Moment::from_millis(
        seconds
            .checked_mul(1_000)
            .and_then(|seconds| seconds.checked_add(millis))
            .context("Time is too large")?,
    ))
}
