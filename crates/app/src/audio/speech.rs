//! Speech detection that decides which audio a recording keeps. Each 20 ms window is classified
//! once while recording, leaving only the final partial window to classify when capture stops.
//! While one of Speakeasy's own cues may sound, a window counts only by its voice band, which a
//! voice fills and the cues leave empty.

use std::{ops::Range, time::Duration};

use super::pcm::{SAMPLE_BYTES, bytes_in, samples_in};
use crate::{lowpass::Lowpass, sounds::HEARD_FOR};

const WINDOW: Duration = Duration::from_millis(20);
const MINIMUM_AUDIBLE: Duration = Duration::from_millis(100);
/// Quiet at either edge must reach this threshold before trimming to the padding.
const QUIET_EDGE: Duration = Duration::from_secs(1);
/// Quiet kept on each side of speech, so soft word edges survive trimming.
const PADDING: Duration = Duration::from_millis(500);
const AUDIBLE_RMS: f64 = 0.003;
/// The top of the voice band in hertz. A voice's fundamental and lowest harmonics lie beneath it,
/// while the cues a recording can hear are high-passed at 400 Hz, leaving it nearly empty.
const VOICE_BAND: f64 = 300.0;

/// Audibility of the windows classified so far. Offsets and lengths count bytes of PCM16 audio.
pub(super) struct Audibility {
    window: usize,
    quiet_edge: usize,
    padding: usize,
    minimum_audible: usize,
    heard_for: usize,
    /// Windows that start before this offset may hold a cue, so they count only by their voice
    /// band.
    cue_until: usize,
    /// Isolates the voice band; `None` at rates too slow to carry anything above it.
    voice: Option<Lowpass>,
    /// Bytes classified: whole windows until the final partial one.
    scanned: usize,
    /// From the start of the first audible window to the end of the last.
    speech: Option<Range<usize>>,
    audible: usize,
}

impl Audibility {
    /// A classifier for audio sampled at `rate` hertz.
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

    /// Classifies every window `audio` has completed since the last call.
    pub(super) fn extend(&mut self, audio: &[u8]) {
        while let Some(window) = audio.get(self.scanned..self.scanned.saturating_add(self.window)) {
            self.classify(window);
        }
    }

    /// Classifies the remaining windows, including a final partial one, once capture has stopped.
    pub(super) fn complete(&mut self, audio: &[u8]) {
        self.extend(audio);
        if let Some(rest) = audio.get(self.scanned..).filter(|rest| !rest.is_empty()) {
            self.classify(rest);
        }
    }

    /// Expects a cue asked for now: the windows not yet classified count only by their voice band
    /// through [`HEARD_FOR`] beyond the end of `audio`, the recording so far.
    pub(super) fn expect_cue(&mut self, audio: &[u8]) {
        let heard_until = audio.len().saturating_add(self.heard_for);
        self.cue_until = self.cue_until.max(heard_until);
    }

    fn classify(&mut self, window: &[u8]) {
        let (mut whole, mut voice) = (0.0, 0.0);
        // The filter runs over every sample, so it is settled whenever a cue arrives.
        for pair in window.as_chunks::<SAMPLE_BYTES>().0 {
            let sample = f64::from(i16::from_le_bytes(*pair)) / 32_768.0;
            let low = self
                .voice
                .as_mut()
                .map_or(sample, |band| band.filter(sample));
            whole += sample * sample;
            voice += low * low;
        }
        let energy = if self.scanned < self.cue_until {
            voice
        } else {
            whole
        };
        let samples = window.len() / SAMPLE_BYTES;
        let end = self.scanned.saturating_add(window.len());
        if energy >= AUDIBLE_RMS.powi(2) * samples as f64 {
            self.speech.get_or_insert(self.scanned..end).end = end;
            self.audible = self.audible.saturating_add(window.len());
        }
        self.scanned = end;
    }

    /// The audio worth recognizing out of `length` bytes: speech with its padding, after trimming
    /// leading and trailing quiet of a second or more while keeping interior pauses.
    /// `None` until 100 ms of audible windows have arrived.
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
