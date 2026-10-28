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
mod tests {
    use std::{f64::consts::TAU, iter};

    use proptest::prelude::*;

    use super::*;
    use crate::{audio::pcm::MAX_SAMPLE_RATE, sounds::Cue};

    const RATE: u32 = 16_000;
    const SECOND: usize = 32_000;

    /// A sine at `hz` on the test rate, peaking at `amplitude` between 5 ms fades, which keep a
    /// struck note's sudden edges out of the voice band the way a cue's recording does.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Each sample stays within the i16 range the amplitude names"
    )]
    fn tone(bytes: usize, hz: f64, amplitude: f64) -> Vec<u8> {
        let samples = bytes / SAMPLE_BYTES;
        let fade = f64::from(RATE) / 200.0;
        encode((0..samples).map(|index| {
            let edge = index.min(samples.saturating_sub(index)) as f64 / fade;
            let envelope = (edge.min(1.0) * TAU / 4.0).sin().powi(2);
            let phase = TAU * hz * index as f64 / f64::from(RATE);
            (phase.sin() * amplitude * envelope) as i16
        }))
    }

    fn audio(segments: &[(usize, i16)]) -> Vec<u8> {
        encode(
            segments
                .iter()
                .flat_map(|&(bytes, amplitude)| iter::repeat_n(amplitude, bytes / SAMPLE_BYTES)),
        )
    }

    fn decode(pcm: &[u8]) -> impl Iterator<Item = i16> {
        pcm.as_chunks::<SAMPLE_BYTES>()
            .0
            .iter()
            .copied()
            .map(i16::from_le_bytes)
    }

    fn encode(samples: impl IntoIterator<Item = i16>) -> Vec<u8> {
        samples.into_iter().flat_map(i16::to_le_bytes).collect()
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
            let mut batch = Audibility::new(rate);
            batch.complete(&recording);
            let mut incremental = Audibility::new(rate);
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
        let window = bytes_in(samples_in(WINDOW, RATE));
        for packet in [
            2,
            14,
            38,
            window - 2,
            window,
            window + 2,
            882,
            recording.len(),
        ] {
            let mut speech = Audibility::new(RATE);
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
            let mut speech = Audibility::new(RATE);
            speech.complete(&recording);
            assert_eq!(speech.retained(recording.len()), Some(start..end));
        }
    }

    #[test]
    fn final_partial_window_counts_towards_the_speech_gate() {
        // At this rate the gate's 1604 samples end inside the sixth 320-sample window.
        let rate = 16_040;
        let millis = |millis| bytes_in(samples_in(Duration::from_millis(millis), rate));
        let mut speech = Audibility::new(rate);
        let too_short = audio(&[(millis(99), 3000)]);
        speech.complete(&too_short);
        assert_eq!(speech.retained(too_short.len()), None);

        let recording = audio(&[(millis(101), 3000)]);
        let mut speech = Audibility::new(rate);
        speech.extend(&recording);
        assert_eq!(speech.retained(recording.len()), None);
        speech.complete(&recording);
        assert_eq!(speech.scanned, recording.len());
        assert_eq!(speech.retained(recording.len()), Some(0..recording.len()));
        speech.complete(&recording);
        assert_eq!(speech.scanned, recording.len());
    }

    /// A full-scale 1 kHz tone lasting `bytes`, then two quiet seconds.
    fn tone_then_quiet(bytes: usize) -> Vec<u8> {
        [tone(bytes, 1_000.0, 32_767.0), audio(&[(SECOND * 2, 0)])].concat()
    }

    #[test]
    fn a_full_scale_tone_counts_as_speech_where_no_cue_may_sound() {
        let recording = tone_then_quiet(bytes_in(samples_in(HEARD_FOR, RATE)));
        let mut speech = Audibility::new(RATE);
        speech.complete(&recording);
        assert!(speech.retained(recording.len()).is_some());
    }

    #[test]
    fn only_what_sounds_beyond_a_cues_span_counts_toward_the_gate() {
        let span = bytes_in(samples_in(HEARD_FOR, RATE));
        for (beyond, passes) in [(0, false), (SECOND * 8 / 100, false), (SECOND / 10, true)] {
            let recording = tone_then_quiet(span + beyond);
            let mut speech = Audibility::new(RATE);
            speech.expect_cue(&[]);
            speech.complete(&recording);
            assert_eq!(
                speech.retained(recording.len()).is_some(),
                passes,
                "{beyond}"
            );
        }
    }

    #[test]
    fn a_voice_beneath_a_cue_still_counts() {
        let quiet = audio(&[(SECOND * 2, 0)]);
        let both = encode(
            decode(&tone(SECOND / 5, 150.0, 3_000.0))
                .zip(decode(&tone(SECOND / 5, 1_000.0, 20_000.0)))
                .map(|(voice, cue)| voice.saturating_add(cue))
                .chain(decode(&quiet)),
        );
        let mut speech = Audibility::new(RATE);
        speech.expect_cue(&[]);
        speech.complete(&both);
        assert!(speech.retained(both.len()).is_some());
    }

    #[test]
    fn the_cues_a_recording_hears_stay_out_of_the_voice_band_even_at_full_scale() {
        for cue in [Cue::Start, Cue::Show] {
            let peak = decode(cue.pcm()).map(i16::unsigned_abs).max().unwrap();
            let loudest = encode(
                decode(cue.pcm())
                    .map(|sample| {
                        i16::try_from(i32::from(sample) * 32_767 / i32::from(peak)).unwrap()
                    })
                    .chain(iter::repeat_n(0, 48_000)),
            );
            let mut speech = Audibility::new(48_000);
            speech.expect_cue(&[]);
            speech.complete(&loudest);
            assert_eq!(speech.audible, 0, "{cue:?}");
        }
    }

    #[test]
    fn a_cue_during_a_recording_counts_only_by_its_voice_band_from_where_it_was_asked_for() {
        let recording = [
            audio(&[(SECOND, 3000), (SECOND, 0)]),
            tone(SECOND / 5, 1_000.0, 3_000.0),
            audio(&[(SECOND * 9 / 5, 0)]),
        ]
        .concat();
        let mut speech = Audibility::new(RATE);
        speech.extend(&recording[..SECOND * 2]);
        speech.expect_cue(&recording[..SECOND * 2]);
        speech.complete(&recording);
        // The trailing edge follows the speech, so the cue goes with the quiet after it.
        assert_eq!(speech.retained(recording.len()), Some(0..SECOND * 3 / 2));
    }

    #[test]
    fn speech_after_a_cue_trims_the_quiet_before_it_cue_and_all() {
        let recording = [
            tone(SECOND * 3 / 10, 1_000.0, 3_000.0),
            audio(&[(SECOND * 12 / 10, 0), (SECOND, 3000), (SECOND * 2, 0)]),
        ]
        .concat();
        let mut speech = Audibility::new(RATE);
        speech.expect_cue(&[]);
        speech.complete(&recording);
        assert_eq!(speech.retained(recording.len()), Some(SECOND..SECOND * 3));
    }

    #[test]
    fn silence_and_clicks_never_count_as_speech() {
        for recording in [
            audio(&[(SECOND, 0)]),
            audio(&[(SECOND / 20, 3000), (SECOND * 2, 0)]),
        ] {
            let mut speech = Audibility::new(RATE);
            speech.extend(&recording);
            speech.complete(&recording);
            assert_eq!(speech.retained(recording.len()), None);
        }
    }
}
