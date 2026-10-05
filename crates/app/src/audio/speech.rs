//! Classifies each 20 ms speech window once while recording, leaving only the final partial
//! window to classify when capture stops.

use std::{ops::Range, time::Duration};

use super::{MAX_SAMPLE_RATE, SAMPLE_BYTES, bytes_in, samples_in};

const WINDOW: Duration = Duration::from_millis(20);
const MINIMUM_AUDIBLE: Duration = Duration::from_millis(100);
/// Quiet at either edge must reach this threshold before trimming to the padding.
const QUIET_EDGE: Duration = Duration::from_secs(1);
/// Quiet kept on each side of speech, so soft word edges survive trimming.
const PADDING: Duration = Duration::from_millis(500);
const AUDIBLE_RMS: f64 = 0.003;

/// Audibility of the windows classified so far. Offsets count bytes of PCM16 audio.
pub(super) struct Speech {
    window: usize,
    quiet_edge: usize,
    padding: usize,
    minimum_audible: usize,
    /// Bytes classified: whole windows until the final partial one.
    scanned: usize,
    first: Option<usize>,
    /// The end of the last audible window.
    end: usize,
    audible_samples: usize,
}

impl Speech {
    /// A classifier for `rate` Hz, or `None` outside 1 Hz–192 kHz.
    pub(super) fn new(rate: u32) -> Option<Self> {
        (1..=MAX_SAMPLE_RATE).contains(&rate).then(|| Self {
            window: bytes_in(samples_in(WINDOW, rate).max(1)),
            quiet_edge: bytes_in(samples_in(QUIET_EDGE, rate)),
            padding: bytes_in(samples_in(PADDING, rate)),
            minimum_audible: samples_in(MINIMUM_AUDIBLE, rate).max(1),
            scanned: 0,
            first: None,
            end: 0,
            audible_samples: 0,
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
    /// leading and trailing quiet of a second or more while keeping interior pauses.
    /// `None` until 100 ms of audible windows have arrived.
    pub(super) fn retained(&self, length: usize) -> Option<Range<usize>> {
        let first = self
            .first
            .filter(|_| self.audible_samples >= self.minimum_audible)?;
        let start = if first >= self.quiet_edge {
            first.saturating_sub(self.padding)
        } else {
            0
        };
        let end = if length.saturating_sub(self.end) >= self.quiet_edge {
            self.end.saturating_add(self.padding).min(length)
        } else {
            length
        };
        Some(start..end)
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
    use proptest::prelude::*;

    use super::*;

    const RATE: u32 = 16_000;
    const SECOND: usize = 32_000;

    fn audio(segments: &[(usize, i16)]) -> Vec<u8> {
        segments
            .iter()
            .flat_map(|&(bytes, amplitude)| {
                std::iter::repeat_n(amplitude.to_le_bytes(), bytes / SAMPLE_BYTES).flatten()
            })
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn packet_splits_do_not_change_whole_recording_trimming(
            rate in prop_oneof![Just(1_u32), Just(49), Just(44_101), Just(MAX_SAMPLE_RATE), 1_u32..=MAX_SAMPLE_RATE],
            segments in prop::collection::vec((0_usize..60, prop_oneof![Just(0_i16), Just(20), Just(98), Just(3000), any::<i16>()]), 0..12),
            leading_ms in prop_oneof![Just(0_u64), Just(499), Just(500), Just(999), Just(1000), Just(1001), Just(2000)],
            trailing_ms in prop_oneof![Just(0_u64), Just(499), Just(500), Just(999), Just(1000), Just(1001), Just(2000)],
            packet_samples in 1_usize..4096,
        ) {
            let mut shaped = vec![(bytes_in(samples_in(Duration::from_millis(leading_ms), rate)), 0)];
            shaped.extend(segments.iter().map(|&(millis, amplitude)| {
                (bytes_in(samples_in(Duration::from_millis(millis as u64), rate)), amplitude)
            }));
            shaped.push((bytes_in(samples_in(Duration::from_millis(trailing_ms), rate)), 0));
            let recording = audio(&shaped);
            let mut batch = Speech::new(rate).unwrap();
            batch.complete(&recording);
            let mut incremental = Speech::new(rate).unwrap();
            for length in (bytes_in(packet_samples)..recording.len()).step_by(bytes_in(packet_samples)) {
                incremental.extend(&recording[..length]);
            }
            incremental.complete(&recording);
            prop_assert_eq!(incremental.retained(recording.len()), batch.retained(recording.len()));
            prop_assert_eq!(incremental.scanned, recording.len());
        }
    }

    #[test]
    fn packet_boundaries_preserve_padding_and_interior_pauses() {
        let recording = audio(&[
            (SECOND * 2, 0),
            (SECOND, 3000),
            (SECOND, 0),
            (SECOND, 3000),
            (SECOND * 2, 0),
        ]);
        // Includes splits inside and on either side of the classifier's 20 ms windows.
        for packet in [2, 14, 38, 638, 640, 642, 882, recording.len()] {
            let mut speech = Speech::new(RATE).unwrap();
            for length in (packet..recording.len()).step_by(packet) {
                speech.extend(&recording[..length]);
            }
            speech.complete(&recording);
            assert_eq!(
                speech.retained(recording.len()),
                Some(SECOND * 3 / 2..SECOND * 11 / 2)
            );
        }
    }

    #[test]
    fn each_quiet_edge_must_reach_one_second_before_trimming() {
        for (leading, trailing, start, end) in [
            (SECOND * 98 / 100, SECOND * 98 / 100, 0, SECOND * 296 / 100),
            (SECOND, SECOND * 98 / 100, SECOND / 2, SECOND * 298 / 100),
            (SECOND * 98 / 100, SECOND, 0, SECOND * 248 / 100),
            (SECOND, SECOND, SECOND / 2, SECOND * 5 / 2),
        ] {
            let recording = audio(&[(leading, 0), (SECOND, 3000), (trailing, 0)]);
            let mut speech = Speech::new(RATE).unwrap();
            speech.complete(&recording);
            assert_eq!(speech.retained(recording.len()), Some(start..end));
        }
    }

    #[test]
    fn final_partial_window_counts_towards_the_speech_gate() {
        let mut speech = Speech::new(RATE).unwrap();
        let too_short = audio(&[(SECOND * 99 / 1000, 3000)]);
        speech.complete(&too_short);
        assert_eq!(speech.retained(too_short.len()), None);

        let recording = audio(&[(SECOND * 101 / 1000, 3000)]);
        let mut speech = Speech::new(RATE).unwrap();
        speech.extend(&recording);
        assert_eq!(speech.scanned, SECOND / 10);
        speech.complete(&recording);
        assert_eq!(speech.scanned, recording.len());
        assert_eq!(speech.retained(recording.len()), Some(0..recording.len()));
        speech.complete(&recording);
        assert_eq!(speech.scanned, recording.len());
    }

    #[test]
    fn silence_and_clicks_never_count_as_speech() {
        for recording in [
            audio(&[(SECOND, 0)]),
            audio(&[(SECOND / 20, 3000), (SECOND * 2, 0)]),
        ] {
            let mut speech = Speech::new(RATE).unwrap();
            speech.extend(&recording);
            speech.complete(&recording);
            assert_eq!(speech.retained(recording.len()), None);
        }
    }
}
