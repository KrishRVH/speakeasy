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
/// Quiet after speech that reports a pause before the full padding: a shortcut released after the
/// last word can take the latest such pause's recognition instead of making its own request.
const EARLY_PAUSES: [Duration; 2] = [Duration::from_millis(100), Duration::from_millis(200)];
const AUDIBLE_RMS: f64 = 0.003;

/// Audibility of the windows classified so far. Offsets count bytes of PCM16 audio.
pub(super) struct Speech {
    window: usize,
    leading_quiet: usize,
    padding: usize,
    early_pauses: [usize; 2],
    minimum_audible: usize,
    /// Bytes classified: whole windows until the final partial one.
    scanned: usize,
    first: Option<usize>,
    /// The end of the last audible window.
    end: usize,
    audible_samples: usize,
    /// `end` when the latest pause was reported, and how many marks it has reported, the full
    /// padding last.
    paused_at: usize,
    paused_marks: usize,
}

/// Quiet after speech: what a recording stopped now would keep, through an early mark or the full
/// padding, and where its speech ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pause {
    pub kept: Range<usize>,
    pub speech_end: usize,
    /// Whether the full padding of quiet has followed the speech.
    pub full: bool,
}

impl Speech {
    /// A classifier for `rate` Hz, or `None` outside 1 Hz–192 kHz.
    pub(super) fn new(rate: u32) -> Option<Self> {
        (1..=MAX_SAMPLE_RATE).contains(&rate).then(|| Self {
            window: bytes_in(samples_in(WINDOW, rate).max(1)),
            leading_quiet: bytes_in(samples_in(LEADING_QUIET, rate)),
            padding: bytes_in(samples_in(PADDING, rate)),
            early_pauses: EARLY_PAUSES.map(|mark| bytes_in(samples_in(mark, rate))),
            minimum_audible: samples_in(MINIMUM_AUDIBLE, rate).max(1),
            scanned: 0,
            first: None,
            end: 0,
            audible_samples: 0,
            paused_at: 0,
            paused_marks: 0,
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

    /// Reports a pause at each mark its quiet reaches: 100 and 200 ms after new speech, keeping that
    /// much, then the full padding, keeping what a recording stopped then would. Marks passed at once
    /// are reported as the latest of them.
    pub(super) fn pause(&mut self) -> Option<Pause> {
        let quiet = self.scanned.saturating_sub(self.end);
        let marks = self
            .early_pauses
            .iter()
            .chain([&self.padding])
            .take_while(|&&mark| quiet >= mark)
            .count();
        let reported = if self.end <= self.paused_at {
            self.paused_marks
        } else {
            0
        };
        if marks <= reported {
            return None;
        }
        let mut kept = self.retained(self.scanned)?;
        let full = marks > self.early_pauses.len();
        if let Some(mark) = self
            .early_pauses
            .get(marks.saturating_sub(1))
            .filter(|_| !full)
        {
            kept.end = self.end.saturating_add(*mark);
        }
        self.paused_at = self.end;
        self.paused_marks = marks;
        Some(Pause {
            kept,
            speech_end: self.end,
            full,
        })
    }

    /// The end of the last audible window.
    pub(super) const fn speech_end(&self) -> usize {
        self.end
    }

    /// Where the next mark this pause has not reported falls due, as a byte offset, once enough
    /// audible audio exists for a pause to report: the first window boundary that much quiet
    /// reaches, where [`Self::pause`] reports it.
    pub(super) fn next_mark(&self) -> Option<usize> {
        self.first
            .filter(|_| self.audible_samples >= self.minimum_audible)?;
        let reported = if self.end <= self.paused_at {
            self.paused_marks
        } else {
            0
        };
        let next = self
            .early_pauses
            .iter()
            .chain([&self.padding])
            .nth(reported)?;
        Some(
            self.end
                .saturating_add(next.div_ceil(self.window).saturating_mul(self.window)),
        )
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
    fn a_pause_reports_each_early_mark_then_what_stopping_after_the_padding_would_keep() {
        let speaking = audio(&[(SECOND * 2, 0), (SECOND, 3000), (SECOND / 2, 0)]);
        let spoken = SECOND * 3;
        let mut live = Speech::new(RATE).unwrap();
        live.extend(&speaking[..SECOND]);
        assert_eq!(live.next_mark(), None, "No speech yet");
        live.extend(&speaking[..spoken]);
        assert_eq!(live.next_mark(), Some(spoken + SECOND / 10));
        live.extend(&speaking[..spoken + SECOND / 10 - 2]);
        assert_eq!(live.pause(), None, "Half a window short of the first mark");
        let mut early = Vec::new();
        for mark in [spoken + SECOND / 10, spoken + SECOND / 5] {
            live.extend(&speaking[..mark]);
            early.push(live.pause().unwrap());
            assert_eq!(live.pause(), None, "Each mark is reported once");
        }
        assert_eq!(early[0].kept.end, spoken + SECOND / 10);
        assert_eq!(early[1].kept.end, spoken + SECOND / 5);
        live.extend(&speaking[..speaking.len() - 2]);
        assert_eq!(live.pause(), None, "Half a window short of the padding");
        live.extend(&speaking);
        let paused = live.pause().unwrap();
        assert!(paused.full);
        assert!(
            early
                .iter()
                .all(|pause| !pause.full && pause.speech_end == paused.speech_end)
        );
        assert_eq!(live.pause(), None, "A pause is reported fully once");
        assert_eq!(live.next_mark(), None, "Every mark was reported");

        let mut stopped_later = speaking.clone();
        stopped_later.extend(audio(&[(SECOND * 3 / 4, 20)]));
        live.complete(&stopped_later);
        assert_eq!(live.speech_end(), paused.speech_end);
        assert_eq!(
            live.retained(stopped_later.len()),
            Some(paused.kept.clone())
        );
        let mut batch = Speech::new(RATE).unwrap();
        batch.complete(&stopped_later);
        assert_eq!(batch.retained(stopped_later.len()), Some(paused.kept));
    }

    #[test]
    fn new_speech_after_a_pause_reports_the_next_pause_and_moves_the_end() {
        let mut recording = audio(&[(SECOND, 3000), (SECOND / 2, 0)]);
        let mut speech = Speech::new(RATE).unwrap();
        speech.extend(&recording);
        let first = speech.pause().unwrap();
        assert!(
            first.full,
            "Quiet that arrives at once is reported only fully"
        );
        recording.extend(audio(&[(SECOND / 5, 3000), (SECOND / 2, 0)]));
        speech.extend(&recording);
        let second = speech.pause().unwrap();
        assert_eq!(second.kept.start, first.kept.start);
        assert!(second.speech_end > first.speech_end);
        speech.complete(&recording);
        assert_eq!(speech.retained(recording.len()), Some(second.kept));
    }

    #[test]
    fn the_next_mark_names_the_window_boundary_where_its_pause_is_reported() {
        // At 11025 Hz a 20 ms window holds 220 samples but 100 ms of quiet is 1102.
        let rate = 11_025;
        let spoken = 50 * 2 * 220;
        let pcm = audio(&[(spoken, 3000), (2 * 11_025, 0)]);
        let mut speech = Speech::new(rate).unwrap();
        speech.extend(&pcm[..spoken]);
        let due = speech.next_mark().unwrap();
        speech.extend(&pcm[..due - 2]);
        assert_eq!(speech.pause(), None);
        speech.extend(&pcm[..due]);
        assert!(speech.pause().is_some(), "The mark was not reported where it fell due");
    }

    #[test]
    fn clicks_never_count_as_speech_or_pauses() {
        let click = audio(&[(SECOND / 20, 3000), (SECOND * 2, 0)]);
        let mut speech = Speech::new(RATE).unwrap();
        speech.extend(&click);
        assert_eq!(speech.pause(), None);
        // A pause the click cannot report must not keep capture waking for its marks.
        assert_eq!(speech.next_mark(), None);
        speech.complete(&click);
        assert_eq!(speech.retained(click.len()), None);
    }
}
