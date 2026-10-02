//! Which part of a recording holds speech, judged in 20 ms windows as audio arrives.
//!
//! Capture classifies each window once while it records. The range kept when recording stops is
//! then known without a rescan, and the range reported at a pause is exactly what a recording that
//! stopped at that moment would keep.

use std::{ops::Range, time::Duration};

use super::{MAX_SAMPLE_RATE, SAMPLE_BYTES, bytes_in, samples_in};

const WINDOW: Duration = Duration::from_millis(20);
const MINIMUM_AUDIBLE: Duration = Duration::from_millis(100);
/// Leading quiet this long or longer is trimmed down to the padding.
const LEADING_QUIET: Duration = Duration::from_secs(1);
/// Quiet kept on each side of speech, so soft word edges survive trimming.
const PADDING: Duration = Duration::from_millis(500);
const AUDIBLE_RMS: f64 = 0.003;

/// Audibility of the windows classified so far. Offsets count bytes of PCM16 audio.
pub(super) struct Speech {
    window: usize,
    leading_quiet: usize,
    padding: usize,
    minimum_audible: usize,
    /// Bytes classified: whole windows until the final partial one.
    scanned: usize,
    first: Option<usize>,
    /// The end of the last audible window.
    end: usize,
    audible_samples: usize,
    /// `end` when the latest pause was reported.
    paused_at: usize,
}

impl Speech {
    /// A classifier for `rate` Hz, or `None` outside 1 Hz–192 kHz.
    pub(super) fn new(rate: u32) -> Option<Self> {
        (1..=MAX_SAMPLE_RATE).contains(&rate).then(|| Self {
            window: bytes_in(samples_in(WINDOW, rate).max(1)),
            leading_quiet: bytes_in(samples_in(LEADING_QUIET, rate)),
            padding: bytes_in(samples_in(PADDING, rate)),
            minimum_audible: samples_in(MINIMUM_AUDIBLE, rate).max(1),
            scanned: 0,
            first: None,
            end: 0,
            audible_samples: 0,
            paused_at: 0,
        })
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

    fn classify(&mut self, window: &[u8]) {
        if is_audible(window) {
            self.first.get_or_insert(self.scanned);
            self.end = self.scanned.saturating_add(window.len());
            self.audible_samples = self
                .audible_samples
                .saturating_add(window.len() / SAMPLE_BYTES);
        }
        self.scanned = self.scanned.saturating_add(window.len());
    }

    /// The audio worth recognizing out of `length` bytes: speech with its padding, after trimming
    /// leading quiet of a second or more and trailing quiet beyond the padding. Interior pauses are
    /// kept. `None` until 100 ms of audible windows have arrived.
    pub(super) fn retained(&self, length: usize) -> Option<Range<usize>> {
        let first = self
            .first
            .filter(|_| self.audible_samples >= self.minimum_audible)?;
        let start = if first >= self.leading_quiet {
            first.saturating_sub(self.padding)
        } else {
            0
        };
        Some(start..self.end.saturating_add(self.padding).min(length))
    }

    /// Once new speech has been followed by a full padding of quiet, the range a recording stopped
    /// now would keep. Each pause is reported once.
    pub(super) fn pause(&mut self) -> Option<Range<usize>> {
        let quiet = self.scanned.saturating_sub(self.end);
        if self.end <= self.paused_at || quiet < self.padding {
            return None;
        }
        let range = self.retained(self.scanned)?;
        self.paused_at = self.end;
        Some(range)
    }
}

fn is_audible(window: &[u8]) -> bool {
    let energy = window
        .as_chunks::<SAMPLE_BYTES>()
        .0
        .iter()
        .map(|pair| u64::from(i16::from_le_bytes(*pair).unsigned_abs()).pow(2))
        // A 20 ms window at 192 kHz holds at most 3840 samples, whose squares sum below 2^42, so
        // wrapping addition is exact and lets the compiler vectorize it.
        .fold(0_u64, u64::wrapping_add);
    let samples = window.len() / SAMPLE_BYTES;
    energy as f64 / (32768.0 * 32768.0) >= AUDIBLE_RMS.powi(2) * samples as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;
    const SECOND: usize = 32_000;

    fn audio(segments: &[(usize, i16)]) -> Vec<u8> {
        segments
            .iter()
            .flat_map(|&(bytes, amplitude)| {
                std::iter::repeat_n(amplitude.to_le_bytes(), bytes / 2).flatten()
            })
            .collect()
    }

    #[test]
    fn trailing_quiet_beyond_the_padding_is_trimmed_and_leading_quiet_only_past_a_second() {
        let recording = audio(&[(SECOND * 8 / 10, 0), (SECOND, 3000), (SECOND * 8 / 10, 0)]);
        let mut speech = Speech::new(RATE).unwrap();
        speech.complete(&recording);
        let end = SECOND * 18 / 10 + SECOND / 2;
        assert_eq!(speech.retained(recording.len()), Some(0..end));
    }

    #[test]
    fn a_pause_reports_exactly_what_stopping_there_would_keep() {
        let speaking = audio(&[(SECOND * 2, 0), (SECOND, 3000), (SECOND / 2, 0)]);
        let mut live = Speech::new(RATE).unwrap();
        live.extend(&speaking[..speaking.len() - 2]);
        assert_eq!(live.pause(), None, "Half a window short of the padding");
        live.extend(&speaking);
        let paused = live.pause().unwrap();
        assert_eq!(live.pause(), None, "A pause is reported once");

        let mut stopped_later = speaking.clone();
        stopped_later.extend(audio(&[(SECOND * 3 / 4, 20)]));
        live.complete(&stopped_later);
        assert_eq!(live.retained(stopped_later.len()), Some(paused.clone()));
        let mut batch = Speech::new(RATE).unwrap();
        batch.complete(&stopped_later);
        assert_eq!(batch.retained(stopped_later.len()), Some(paused));
    }

    #[test]
    fn new_speech_after_a_pause_reports_the_next_pause_and_moves_the_end() {
        let mut recording = audio(&[(SECOND, 3000), (SECOND / 2, 0)]);
        let mut speech = Speech::new(RATE).unwrap();
        speech.extend(&recording);
        let first = speech.pause().unwrap();
        recording.extend(audio(&[(SECOND / 5, 3000), (SECOND / 2, 0)]));
        speech.extend(&recording);
        let second = speech.pause().unwrap();
        assert_eq!(second.start, first.start);
        assert!(second.end > first.end);
        speech.complete(&recording);
        assert_eq!(speech.retained(recording.len()), Some(second));
    }

    #[test]
    fn clicks_never_count_as_speech_or_pauses() {
        let click = audio(&[(SECOND / 20, 3000), (SECOND * 2, 0)]);
        let mut speech = Speech::new(RATE).unwrap();
        speech.extend(&click);
        assert_eq!(speech.pause(), None);
        speech.complete(&click);
        assert_eq!(speech.retained(click.len()), None);
    }
}
