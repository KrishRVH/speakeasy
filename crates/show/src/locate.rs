//! Locating spoken phrases and marks without inventing times or returning private text.

mod evidence;

use std::{fmt, time::Duration};

pub use evidence::{Cell, Evidence, PixelRect, crop_image};

use crate::{Manifest, Moment, Narration, Span};

/// An explicit location to investigate in one Show. Phrase contents never appear in diagnostics.
#[derive(Clone, Copy)]
pub enum Focus<'a> {
    /// A point on the recording's clock.
    At(Moment),
    /// A numbered ink gesture or pin.
    Mark(usize),
    /// Contiguous spoken words, ignoring case and punctuation.
    Phrase(&'a str),
}

impl fmt::Debug for Focus<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::At(at) => out.debug_tuple("At").field(at).finish(),
            Self::Mark(number) => out.debug_tuple("Mark").field(number).finish(),
            Self::Phrase(_) => out.write_str("Phrase(..)"),
        }
    }
}

/// A content-free explanation of why a location cannot be established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unlocated(&'static str);

impl fmt::Display for Unlocated {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.0)
    }
}

impl std::error::Error for Unlocated {}

impl Manifest {
    /// Finds every matching span, in narration order. No match is an empty vector. Matching
    /// searches recorded narration, including phrase corrections, rather than summary edits.
    ///
    /// # Errors
    /// Rejects missing marks, out-of-range times, empty or oversized phrases, unavailable word
    /// timing, and invalid narration timing. Errors never contain narration or the query.
    pub fn locate(&self, focus: Focus<'_>) -> Result<Vec<Span>, Unlocated> {
        let duration = self.recording().duration;
        let spans = match focus {
            Focus::At(at) if at <= duration => vec![Span::at(at)],
            Focus::At(_) => return Err(Unlocated("The requested time is past the Show's end")),
            Focus::Mark(number) => vec![
                self.marks()
                    .iter()
                    .find(|mark| mark.number == number)
                    .ok_or(Unlocated("This Show has no mark with that number"))?
                    .span,
            ],
            Focus::Phrase(phrase) => self.find_phrase(phrase)?,
        };
        if spans.iter().any(|span| span.end() > duration) {
            return Err(Unlocated(
                "The location is outside the recorded screen timeline",
            ));
        }
        Ok(spans)
    }

    fn find_phrase(&self, phrase: &str) -> Result<Vec<Span>, Unlocated> {
        if phrase.len() > 2_048 || phrase.chars().any(char::is_control) {
            return Err(Unlocated("Use a phrase of at most 2048 bytes on one line"));
        }
        let phrase: Vec<_> = tokens(phrase).collect();
        if phrase.is_empty() || phrase.len() > 64 {
            return Err(Unlocated("Use a phrase containing between 1 and 64 words"));
        }
        let words = match &self.recording().narration {
            Narration::Words(words) if !words.is_empty() => words,
            Narration::Words(_) => return Err(Unlocated("This Show has no recorded narration")),
            Narration::Text(_) => {
                return Err(Unlocated(
                    "This Show's narration has no word timings; use a mark or time",
                ));
            },
        };
        if words
            .array_windows::<2>()
            .any(|[one, two]| one.span.start() > two.span.start())
        {
            return Err(Unlocated(
                "Narration word timings are not in recording order",
            ));
        }
        let spoken: Vec<_> = words
            .iter()
            .flat_map(|word| tokens(&word.text).map(move |token| (token, word.span)))
            .take(10_001)
            .collect();
        if spoken.len() > 10_000 {
            return Err(Unlocated(
                "Narration is too large to search; use a mark or time",
            ));
        }
        Ok(spoken
            .windows(phrase.len())
            .filter(|window| window.iter().map(|(text, _)| text).eq(&phrase))
            .filter_map(|window| window.iter().map(|(_, span)| *span).reduce(Span::union))
            .collect())
    }
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
}

/// Widens a location equally on both sides, clamped to the recording's clock.
#[must_use]
pub fn window(span: Span, around: Duration, duration: Moment) -> Span {
    let before = u64::try_from(around.as_millis()).unwrap_or(u64::MAX);
    Span::new(
        Moment::from_millis(span.start().as_millis().saturating_sub(before)).min(duration),
        span.end().saturating_add(around).min(duration),
    )
}

/// Retained samples near a location. Gaps describe the full source sampling, before thinning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Samples {
    /// Distinct actual sample times in recording order.
    pub times: Vec<Moment>,
    /// The held sample carried in when none falls exactly at the window start. It may be older
    /// than the window's whole width.
    pub carried_in: Option<Moment>,
    /// Largest unsampled interval inside the window, including its edges.
    pub longest_gap: Duration,
}

/// Selects at most `most` samples from ordered, distinct retained `moments`.
///
/// Prefers the sample
/// at or before `anchor`, then the carried-in sample and the last sample, and spreads the rest
/// evenly. A window before capture began has no samples; it never borrows a future frame.
#[must_use]
pub fn pick(moments: &[Moment], window: Span, anchor: Moment, most: usize) -> Samples {
    let start = moments.partition_point(|at| *at < window.start());
    let end = moments.partition_point(|at| *at <= window.end());
    let carried_in = start
        .checked_sub(1)
        .and_then(|index| moments.get(index))
        .copied()
        .filter(|_| moments.get(start) != Some(&window.start()));
    let available = moments
        .get(start.saturating_sub(usize::from(carried_in.is_some()))..end)
        .unwrap_or_default();
    let longest_gap = moments
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .copied()
        .chain([window.end()])
        .scan(window.start(), |previous, at| {
            let gap = at.since(*previous);
            *previous = at;
            Some(gap)
        })
        .max()
        .unwrap_or_default();
    let mut chosen = Vec::new();
    let limit = most.min(available.len());
    if limit == available.len() {
        return Samples {
            times: available.to_vec(),
            carried_in,
            longest_gap,
        };
    }
    if limit > 0 {
        chosen.push(
            available
                .partition_point(|at| *at <= anchor)
                .saturating_sub(1),
        );
        for index in [0, available.len().saturating_sub(1)] {
            if chosen.len() < limit && !chosen.contains(&index) {
                chosen.push(index);
            }
        }
        // Bisect the largest remaining interval, avoiding a bias toward early samples.
        while chosen.len() < limit {
            chosen.sort_unstable();
            let next = chosen
                .array_windows::<2>()
                .max_by_key(|[one, two]| two.saturating_sub(*one))
                .map(|[one, two]| one.midpoint(*two));
            let Some(next) = next.filter(|next| !chosen.contains(next)) else {
                break;
            };
            chosen.push(next);
        }
    }
    chosen.sort_unstable();
    Samples {
        times: chosen
            .into_iter()
            .filter_map(|index| available.get(index).copied())
            .collect(),
        carried_in,
        longest_gap,
    }
}

#[cfg(test)]
mod tests;
