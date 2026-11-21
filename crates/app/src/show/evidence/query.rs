use anyhow::Context as _;
use serde::Serialize;
use speakeasy_show::{Manifest, Moment, Narration, Span, locate::Focus};

#[derive(PartialEq, Eq)]
pub(in crate::show) enum Query {
    At(Moment),
    Mark(usize),
    Phrase(String),
    Match { phrase: String, occurrence: usize },
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Kind {
    Time,
    Mark,
    Phrase,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::show) struct Found {
    kind: Kind,
    number: Option<usize>,
    #[serde(rename = "match")]
    occurrence: Option<usize>,
    pub matches: Vec<Span>,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
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
        if matches.is_empty() {
            let recording = manifest.recording();
            let outlasts = matches!(&recording.narration, Narration::Words(words)
                if words.iter().any(|word| word.span.start() > recording.duration));
            anyhow::bail!(if outlasts {
                "No matching phrase before the recorded screen timeline ends; later narration is outside the timeline. Use another phrase, mark, or time"
            } else {
                "No matching phrase in the recorded narration; use another phrase, mark, or time"
            });
        }
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

    pub(in crate::show) fn resolve_in_video(&self, manifest: &Manifest) -> anyhow::Result<Found> {
        let Self::At(at) = self else {
            return self.resolve(manifest);
        };
        let span = Span::at(*at);
        Ok(Found {
            kind: Kind::Time,
            number: None,
            occurrence: None,
            matches: vec![span],
            start_ms: Some(at.as_millis()),
            end_ms: Some(at.as_millis()),
            selected: Some(span),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phrase_spoken_only_after_the_screen_timeline_is_reported_outside_it() -> anyhow::Result<()>
    {
        for (closing, phrase, outside) in [
            ([600, 700], "closing words", true),
            ([600, 700], "absent", true),
            ([300, 400], "absent", false),
        ] {
            let manifest = Manifest::parse(
                format!(
                    r#"{{
                    "version":3,"duration":500,"surface":{{"width":4,"height":2}},
                    "strokes":[],"pins":[],"actions":[],"marks":[],
                    "narration":{{"words":[
                        {{"span":[100,200],"text":"opening"}},
                        {{"span":{closing:?},"text":"closing words"}}
                    ]}},
                    "keyframes":[{{"index":1,"at":250,"reason":"middle","event":"fixture","marks":[0,0]}}]
                }}"#
                )
                .as_bytes(),
            )?;
            let error = Query::Phrase(phrase.to_owned())
                .resolve(&manifest)
                .err()
                .context("An unspoken phrase was located")?;
            assert_eq!(
                error.to_string().contains("outside the timeline"),
                outside,
                "{error}"
            );
        }
        Ok(())
    }

    #[test]
    fn times_are_whole_seconds_or_minutes_with_up_to_three_decimals() -> anyhow::Result<()> {
        for (text, ms) in [("0", 0), ("12.5", 12_500), ("1:02.005", 62_005)] {
            assert_eq!(time(text)?.as_millis(), ms);
        }
        for text in [
            "",
            "-1",
            "NaN",
            "1:60",
            "1:2:3",
            "1.",
            ".1",
            "1.1234",
            "1e3",
            "18446744073709551615",
        ] {
            assert!(time(text).is_err(), "accepted {text}");
        }
        Ok(())
    }
}
