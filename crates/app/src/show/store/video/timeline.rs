use std::{
    io::{self, Read as _, Write},
    time::Duration,
};

use anyhow::Context as _;
use speakeasy_core::gesture::RECORDING_LIMIT;
use speakeasy_show::{Narration, Recording, Span, timed_clauses};

use crate::show::store::{checkpoint::KeptAudio, movie::Inspected};

pub(super) const FPS: u64 = 30;

pub(super) struct Timing {
    pub start_ms: u64,
    pub end_ms: u64,
    pub frames: u64,
}

impl Timing {
    pub(super) fn new(
        movie: &Inspected,
        span: Option<Span>,
        audio: Option<&KeptAudio>,
    ) -> anyhow::Result<Self> {
        let limit = u64::try_from(RECORDING_LIMIT.as_millis())?;
        let origin = audio.map_or(movie.metadata.origin_ms, |audio| {
            movie
                .metadata
                .origin_ms
                .min(u64::try_from(audio.start.as_millis()).unwrap_or(u64::MAX))
        });
        let coverage = movie
            .metadata
            .coverage
            .as_ref()
            .context("Movie has no saved coverage")?;
        let start_ms = span.map_or(origin, |span| span.start().as_millis().max(origin));
        let end = audio.map_or(coverage.end_ms, |audio| {
            let audio_end = u64::try_from(audio.start.as_millis())
                .unwrap_or(u64::MAX)
                .saturating_add(
                    u64::try_from(audio.pcm.len() / 2)
                        .unwrap_or(u64::MAX)
                        .saturating_mul(1000)
                        .div_ceil(u64::from(audio.rate.max(1))),
                );
            coverage.end_ms.max(audio_end).min(limit)
        });
        let end_ms = span.map_or(end, |span| span.end().as_millis().min(end));
        anyhow::ensure!(
            start_ms < end_ms,
            "The selected interval has no retained video; choose a wider --around interval"
        );
        let frames = end_ms
            .saturating_sub(start_ms)
            .saturating_mul(FPS)
            .div_ceil(1000);
        Ok(Self {
            start_ms,
            end_ms,
            frames,
        })
    }

    pub(super) fn tick(&self, index: u64) -> Duration {
        Duration::from_millis(self.start_ms).saturating_add(Duration::from_nanos(
            index.saturating_mul(1_000_000_000) / FPS,
        ))
    }

    pub(super) fn duration(&self) -> Duration {
        Duration::from_nanos(self.frames.saturating_mul(1_000_000_000) / FPS)
    }
}

pub(super) fn audio(
    audio: &KeptAudio,
    timing: &Timing,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let sample_at = |time: Duration| -> anyhow::Result<usize> {
        Ok(usize::try_from(
            time.as_nanos().saturating_mul(u128::from(audio.rate)) / 1_000_000_000,
        )?)
    };
    let start = Duration::from_millis(timing.start_ms);
    let samples = usize::try_from(timing.frames.saturating_mul(u64::from(audio.rate)) / FPS)?;
    let content = sample_at(Duration::from_millis(
        timing.end_ms.saturating_sub(timing.start_ms),
    ))?
    .min(samples);
    let padding = sample_at(audio.start.saturating_sub(start))?.min(content);
    let offset = sample_at(start.saturating_sub(audio.start))?.saturating_mul(2);
    zeros(out, padding.saturating_mul(2))?;
    let available = audio.pcm.get(offset..).unwrap_or_default();
    let bytes = content.saturating_sub(padding).saturating_mul(2);
    let copied = available.len().min(bytes);
    out.write_all(
        available
            .get(..copied)
            .context("Voice interval exceeds its recording")?,
    )?;
    zeros(out, bytes.saturating_sub(copied))?;
    zeros(out, samples.saturating_sub(content).saturating_mul(2))?;
    Ok(())
}

fn zeros(out: &mut impl Write, bytes: usize) -> anyhow::Result<()> {
    io::copy(&mut io::repeat(0).take(u64::try_from(bytes)?), out)?;
    Ok(())
}

pub(super) fn captions(
    recording: &Recording,
    timing: &Timing,
    out: &mut impl Write,
) -> anyhow::Result<usize> {
    let Narration::Words(words) = &recording.narration else {
        return Ok(0);
    };
    let words: Vec<_> = words
        .iter()
        .filter(|word| {
            word.span.end().as_millis() > timing.start_ms
                && word.span.start().as_millis() < timing.end_ms
        })
        .cloned()
        .collect();
    let clauses = timed_clauses(&words);
    let mut count = 0_usize;
    for (index, clause) in clauses.iter().enumerate() {
        let next = clauses
            .get(index.saturating_add(1))
            .map_or(timing.end_ms, |next| next.at.as_millis());
        let last_word = words
            .iter()
            .filter(|word| word.span.start() >= clause.at && word.span.start().as_millis() < next)
            .map(|word| word.span.end().as_millis())
            .max()
            .unwrap_or(next);
        let start = clause.at.as_millis().max(timing.start_ms);
        let end = last_word.min(next).min(timing.end_ms);
        if start >= end {
            continue;
        }
        count = count.saturating_add(1);
        let text = clause.text.split_whitespace().collect::<Vec<_>>().join(" ");
        writeln!(
            out,
            "{count}\n{} --> {}",
            timestamp(start.saturating_sub(timing.start_ms)),
            timestamp(end.saturating_sub(timing.start_ms))
        )?;
        for character in text.chars() {
            write!(out, "{character}")?;
            if matches!(character, '{' | '\\') {
                out.write_all(br"{\r}")?;
            }
        }
        out.write_all(b"\n\n")?;
    }
    Ok(count)
}

fn timestamp(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
