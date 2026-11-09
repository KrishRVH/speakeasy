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
        let samples = || {
            window
                .as_chunks::<SAMPLE_BYTES>()
                .0
                .iter()
                .map(|pair| f64::from(i16::from_le_bytes(*pair)) / 32_768.0)
        };
        let whole: AcPower = samples().collect();
        let voice = match self.voice.as_mut() {
            Some(band) => {
                if self.scanned == 0 {
                    band.settle(whole.mean());
                }
                samples()
                    .map(|sample| band.filter(sample))
                    .collect::<AcPower>()
                    .mean_square()
            },
            None => whole.mean_square(),
        };
        let whole = whole.mean_square();
        let floor = AUDIBLE_RMS.powi(2);
        let audible = if self.scanned < self.cue_until {
            voice >= CUE_VOICE_RMS.powi(2)
                || (whole >= floor && voice >= floor && voice >= whole * MINIMUM_VOICE_FRACTION)
        } else {
            whole >= floor
        };
        let end = self.scanned.saturating_add(window.len());
        if audible {
            self.speech.get_or_insert(self.scanned..end).end = end;
            self.audible = self.audible.saturating_add(window.len());
        }
        self.scanned = end;
    }

    pub(super) fn paused(&self, length: usize) -> Option<Range<usize>> {
        let speech = self.speech.as_ref()?;
        if length.saturating_sub(speech.end) < self.quiet_edge {
            return None;
        }
        self.retained(length)
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

#[derive(Default)]
pub(super) struct AcPower {
    sum: f64,
    squares: f64,
    samples: u32,
}

impl AcPower {
    pub(super) fn add(&mut self, sample: f64) {
        self.sum += sample;
        self.squares += sample * sample;
        self.samples = self.samples.saturating_add(1);
    }

    fn mean(&self) -> f64 {
        self.sum / f64::from(self.samples.max(1))
    }

    pub(super) fn mean_square(&self) -> f64 {
        let mean = self.mean();
        (self.squares / f64::from(self.samples.max(1)) - mean * mean).max(0.0)
    }
}

impl FromIterator<f64> for AcPower {
    fn from_iter<I: IntoIterator<Item = f64>>(samples: I) -> Self {
        let mut power = Self::default();
        samples.into_iter().for_each(|sample| power.add(sample));
        power
    }
}

#[cfg(test)]
mod tests;
