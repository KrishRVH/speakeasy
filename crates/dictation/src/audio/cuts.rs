//! Where a recording divides into segments recognized while it continues, the tail a stop would
//! hold, and which speculated pauses can stand in for that tail. Offsets count bytes of PCM16 audio,
//! as [`Speech`](super::speech::Speech) reports them; a segment plus every later segment and the
//! tail is exactly the recording's kept audio.

use std::ops::Range;

use super::speech::Pause;

/// What a pause asks of the capture thread.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AtPause {
    /// Hand the owner this audio as segment `index`, then report it with [`Cuts::segment_sent`].
    Segment(u32, Range<usize>),
    /// Offer this tail for speculative recognition as pause `sequence`.
    Speculate(u32, Range<usize>),
    Nothing,
}

pub(super) struct Cuts {
    /// Tail bytes a pause commits as a segment.
    segment: usize,
    /// Where the audio after the latest sent segment begins.
    committed: Option<usize>,
    segments: u32,
    /// The latest pause offered for speculation.
    sequence: Option<u32>,
    /// Pauses offered since the speech last ended, which a stop with nothing audible after them may
    /// take as its text, oldest first.
    standing: Vec<u32>,
    /// Where that speech ended.
    standing_end: usize,
}

impl Cuts {
    pub(super) const fn new(segment: usize) -> Self {
        Self {
            segment,
            committed: None,
            segments: 0,
            sequence: None,
            standing: Vec::new(),
            standing_end: 0,
        }
    }

    /// A pause becomes a segment once its full padding has followed enough speech since the last
    /// segment; otherwise, while speculating, its tail is offered for recognition.
    pub(super) fn at_pause(&self, pause: Pause, speculating: bool) -> AtPause {
        let tail = self.tail_of(pause.kept);
        if pause.full && tail.len() >= self.segment {
            return AtPause::Segment(self.segments, tail);
        }
        let sequence = self
            .sequence
            .map_or(Some(0), |sequence| sequence.checked_add(1));
        match sequence {
            Some(sequence) if speculating && !tail.is_empty() => AtPause::Speculate(sequence, tail),
            _ => AtPause::Nothing,
        }
    }

    /// The owner received `segment`; later tails begin where it ends.
    pub(super) fn segment_sent(&mut self, segment: &Range<usize>) {
        self.committed = Some(segment.end);
        self.segments = self.segments.saturating_add(1);
        self.standing.clear();
    }

    /// The owner received pause `sequence`, whose speech ended at `speech_end`.
    pub(super) fn speculation_sent(&mut self, sequence: u32, speech_end: usize) {
        if speech_end != self.standing_end {
            self.standing.clear();
            self.standing_end = speech_end;
        }
        self.sequence = Some(sequence);
        self.standing.push(sequence);
    }

    /// Whether speech ended at `speech_end` resumed after the standing pauses, which it withdraws;
    /// reported once.
    pub(super) fn resumed(&mut self, speech_end: usize) -> bool {
        let resumed = !self.standing.is_empty() && speech_end != self.standing_end;
        if resumed {
            self.standing.clear();
        }
        resumed
    }

    /// The tail of a stopped recording that keeps `kept`, if any speech followed the last segment,
    /// and the speculated pauses that nothing audible followed: a stop then would have kept the
    /// same speech with less of its quiet, so their text stands in for the tail's.
    pub(super) fn finish(
        &mut self,
        kept: Option<Range<usize>>,
        speech_end: usize,
    ) -> (Option<Range<usize>>, Vec<u32>) {
        let tail = kept
            .map(|kept| self.tail_of(kept))
            .filter(|tail| !tail.is_empty());
        let standing = if tail.is_some() && speech_end == self.standing_end {
            std::mem::take(&mut self.standing)
        } else {
            Vec::new()
        };
        (tail, standing)
    }

    pub(super) const fn segments(&self) -> u32 {
        self.segments
    }

    fn tail_of(&self, kept: Range<usize>) -> Range<usize> {
        self.committed.map_or(kept.start, |at| at.max(kept.start))..kept.end
    }
}

#[cfg(test)]
mod tests {
    use super::{super::speech::Speech, *};

    const RATE: u32 = 16_000;
    const SECOND: usize = 32_000;

    /// Segments, speculated pauses, then the tail and the pauses that may stand in for it.
    type Recorded = (
        Vec<Range<usize>>,
        Vec<(u32, Range<usize>)>,
        Option<Range<usize>>,
        Vec<u32>,
    );

    /// PCM16 holding `(seconds, amplitude)` spans in order.
    fn audio(spans: &[(usize, i16)]) -> Vec<u8> {
        spans
            .iter()
            .flat_map(|&(seconds, amplitude)| {
                let samples = seconds.checked_mul(usize::try_from(RATE).unwrap()).unwrap();
                std::iter::repeat_n(amplitude.to_le_bytes(), samples).flatten()
            })
            .collect()
    }

    /// Feeds `pcm` in 16 ms reads, as the capture thread drains it, sending every segment and
    /// speculation a pause asks for; returns them with the tail and speculated pause at the end.
    fn record(pcm: &[u8], speculating: bool) -> Recorded {
        let mut speech = Speech::new(RATE).unwrap();
        let mut cuts = Cuts::new(20 * SECOND);
        let (mut segments, mut pauses) = (Vec::new(), Vec::new());
        for end in (512..pcm.len()).step_by(512).chain([pcm.len()]) {
            speech.extend(&pcm[..end]);
            let Some(pause) = speech.pause() else {
                continue;
            };
            let speech_end = pause.speech_end;
            match cuts.at_pause(pause, speculating) {
                AtPause::Segment(index, segment) => {
                    assert_eq!(index as usize, segments.len());
                    cuts.segment_sent(&segment);
                    segments.push(segment);
                },
                AtPause::Speculate(sequence, tail) => {
                    cuts.speculation_sent(sequence, speech_end);
                    pauses.push((sequence, tail));
                },
                AtPause::Nothing => {},
            }
        }
        speech.complete(pcm);
        let (tail, speculated) = cuts.finish(speech.retained(pcm.len()), speech.speech_end());
        assert_eq!(cuts.segments() as usize, segments.len());
        (segments, pauses, tail, speculated)
    }

    #[test]
    fn segments_and_the_tail_are_the_kept_audio_without_gaps_or_overlap() {
        let pcm = audio(&[
            (2, 0),
            (25, 3000),
            (1, 0),
            (10, 3000),
            (1, 0),
            (15, 3000),
            (1, 0),
            (4, 3000),
            (2, 0),
        ]);
        let mut speech = Speech::new(RATE).unwrap();
        speech.complete(&pcm);
        let kept = speech.retained(pcm.len()).unwrap();

        let (segments, _, tail, _) = record(&pcm, false);
        // 25 s of speech fills a segment at its pause; 10 s does not, so the next segment holds it
        // with the 15 s that follow; the last 4 s stay in the tail.
        assert_eq!(segments.len(), 2);
        let tail = tail.unwrap();
        let mut covered = segments.clone();
        covered.push(tail.clone());
        assert_eq!(covered[0].start, kept.start);
        for pair in covered.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "Segments must abut");
        }
        assert_eq!(tail.end, kept.end);
        assert!(segments.iter().all(|segment| segment.len() >= 20 * SECOND));
        assert!(tail.len() < 20 * SECOND);
    }

    #[test]
    fn a_recording_ending_at_a_segment_has_no_tail() {
        let pcm = audio(&[(25, 3000), (3, 0)]);
        let (segments, _, tail, speculated) = record(&pcm, true);
        assert_eq!(segments.len(), 1);
        assert_eq!(tail, None);
        assert_eq!(speculated, Vec::<u32>::new());
    }

    #[test]
    fn short_recordings_are_never_divided() {
        let pcm = audio(&[(1, 0), (8, 3000), (1, 0), (8, 3000), (1, 0)]);
        let (segments, pauses, tail, speculated) = record(&pcm, true);
        assert_eq!(segments, []);
        // Each pause is offered with 100 and 200 ms of its quiet, and again with the full padding.
        assert_eq!(pauses.len(), 6);
        assert_eq!(pauses[3].1.end + 100 * SECOND / 1000, pauses[4].1.end);
        assert_eq!(pauses[4].1.end + 300 * SECOND / 1000, pauses[5].1.end);
        // Stopping in the final pause keeps its full padding, and any of its speculations can stand
        // in: nothing audible followed them.
        assert_eq!(speculated, [3, 4, 5]);
        assert_eq!(tail, Some(pauses[5].1.clone()));
    }

    #[test]
    fn speech_after_a_pause_withdraws_its_speculations() {
        let pcm = audio(&[(8, 3000), (1, 0), (2, 3000)]);
        let (_, pauses, tail, speculated) = record(&pcm, true);
        assert_eq!(pauses.len(), 3);
        assert!(tail.is_some());
        assert_eq!(speculated, Vec::<u32>::new());
    }

    #[test]
    fn resumed_speech_is_reported_once_per_withdrawal() {
        let pcm = audio(&[(8, 3000), (1, 0), (2, 3000), (1, 0)]);
        let mut speech = Speech::new(RATE).unwrap();
        let mut cuts = Cuts::new(20 * SECOND);
        let mut resumed = 0;
        for end in (512..pcm.len()).step_by(512).chain([pcm.len()]) {
            speech.extend(&pcm[..end]);
            resumed += usize::from(cuts.resumed(speech.speech_end()));
            if let Some(pause) = speech.pause() {
                let speech_end = pause.speech_end;
                if let AtPause::Speculate(sequence, _) = cuts.at_pause(pause, true) {
                    cuts.speculation_sent(sequence, speech_end);
                }
            }
        }
        // Only the first pause's speech resumed; the second pause still stands.
        assert_eq!(resumed, 1);
        speech.complete(&pcm);
        let (_, standing) = cuts.finish(speech.retained(pcm.len()), speech.speech_end());
        assert_eq!(standing, [3, 4, 5]);
    }

    #[test]
    fn speculation_after_a_segment_covers_only_the_tail() {
        let pcm = audio(&[(22, 3000), (1, 0), (3, 3000), (1, 0)]);
        let (segments, pauses, tail, speculated) = record(&pcm, true);
        assert_eq!(segments.len(), 1);
        // The first pause's early offers predate its segment; the second pause covers the tail.
        assert_eq!(pauses.len(), 5);
        assert_eq!(pauses[2].1.start, segments[0].end);
        assert_eq!(speculated, [2, 3, 4]);
        assert_eq!(tail, Some(pauses[4].1.clone()));
    }

    #[test]
    fn an_unsent_segment_stays_in_the_tail() {
        let pcm = audio(&[(25, 3000), (1, 0), (2, 3000), (1, 0)]);
        let mut speech = Speech::new(RATE).unwrap();
        let mut cuts = Cuts::new(20 * SECOND);
        for end in (512..pcm.len()).step_by(512).chain([pcm.len()]) {
            speech.extend(&pcm[..end]);
            if let Some(pause) = speech.pause().filter(|pause| pause.full) {
                // A full event lane: the capture thread drops the segment instead of sending it.
                assert!(matches!(
                    cuts.at_pause(pause, false),
                    AtPause::Segment(0, _)
                ));
            }
        }
        speech.complete(&pcm);
        let kept = speech.retained(pcm.len());
        assert_eq!(
            cuts.finish(kept.clone(), speech.speech_end()),
            (kept, Vec::new())
        );
        assert_eq!(cuts.segments(), 0);
    }
}
