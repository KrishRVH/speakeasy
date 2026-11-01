use std::{ops::Range, time::Duration};

use super::pcm::{SAMPLE_BYTES, bytes_in, samples_in};
use crate::{lowpass::Lowpass, sounds::HEARD_FOR};

const WINDOW: Duration = Duration::from_millis(20);
const MINIMUM_AUDIBLE: Duration = Duration::from_millis(100);

const QUIET_EDGE: Duration = Duration::from_secs(1);

const PADDING: Duration = Duration::from_millis(500);
const AUDIBLE_RMS: f64 = 0.0003;
const CUE_VOICE_RMS: f64 = 0.003;
const MINIMUM_VOICE_FRACTION: f64 = 0.01;

const VOICE_BAND: f64 = 300.0;

pub(super) struct Audibility {
    window: usize,
    quiet_edge: usize,
    padding: usize,
    minimum_audible: usize,
    heard_for: usize,
    cue_until: usize,
    voice: Option<Lowpass>,
    scanned: usize,
    speech: Option<Range<usize>>,
    audible: usize,
}

impl Audibility {
    pub(super) fn paused(&self, length: usize) -> Option<Range<usize>> {
        let speech = self.speech.as_ref()?;
        (length.saturating_sub(speech.end) >= self.quiet_edge)
            .then(|| self.retained(length))
            .flatten()
    }

    pub(super) fn new(rate: u32) -> Self {
        Self {
            window: bytes_in(samples_in(WINDOW, rate).max(1)),
            quiet_edge: bytes_in(samples_in(QUIET_EDGE, rate)),
            padding: bytes_in(samples_in(PADDING, rate)),
            minimum_audible: bytes_in(samples_in(MINIMUM_AUDIBLE, rate).max(1)),
            heard_for: bytes_in(samples_in(HEARD_FOR, rate)),
            cue_until: 0,
            voice: Lowpass::new(VOICE_BAND, f64::from(rate)),
            scanned: 0,
            speech: None,
            audible: 0,
        }
    }

    pub(super) fn extend(&mut self, audio: &[u8]) {
        while let Some(window) = audio.get(self.scanned..self.scanned.saturating_add(self.window)) {
            self.classify(window);
        }
    }

    pub(super) fn complete(&mut self, audio: &[u8]) {
        self.extend(audio);
        if let Some(rest) = audio.get(self.scanned..).filter(|rest| !rest.is_empty()) {
            self.classify(rest);
        }
    }

    pub(super) fn expect_cue(&mut self, audio: &[u8]) {
        let heard_until = audio.len().saturating_add(self.heard_for);
        self.cue_until = self.cue_until.max(heard_until);
    }

    fn classify(&mut self, window: &[u8]) {
        let (mut whole, mut voice) = (0.0, 0.0);

        for pair in window.as_chunks::<SAMPLE_BYTES>().0 {
            let sample = f64::from(i16::from_le_bytes(*pair)) / 32_768.0;
            let low = self
                .voice
                .as_mut()
                .map_or(sample, |band| band.filter(sample));
            whole += sample * sample;
            voice += low * low;
        }
        let samples = (window.len() / SAMPLE_BYTES) as f64;
        let minimum = AUDIBLE_RMS.powi(2) * samples;
        let audible = if self.scanned < self.cue_until {
            voice >= CUE_VOICE_RMS.powi(2) * samples
                || (whole >= minimum && voice >= minimum && voice >= whole * MINIMUM_VOICE_FRACTION)
        } else {
            whole >= minimum
        };
        let end = self.scanned.saturating_add(window.len());
        if audible {
            self.speech.get_or_insert(self.scanned..end).end = end;
            self.audible = self.audible.saturating_add(window.len());
        }
        self.scanned = end;
    }

    pub(super) fn retained(&self, length: usize) -> Option<Range<usize>> {
        let speech = self
            .speech
            .as_ref()
            .filter(|_| self.audible >= self.minimum_audible)?;
        let start = if speech.start >= self.quiet_edge {
            speech.start.saturating_sub(self.padding)
        } else {
            0
        };
        let end = if length.saturating_sub(speech.end) >= self.quiet_edge {
            speech.end.saturating_add(self.padding).min(length)
        } else {
            length
        };
        Some(start..end)
    }
}

#[cfg(test)]
mod tests;
