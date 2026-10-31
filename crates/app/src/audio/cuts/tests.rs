//! Every accepted piece accounts for original PCM, without changing final sealing or cue gating.

use proptest::prelude::*;

use super::*;
use crate::audio::pcm::Pcm16;

fn audio(bursts: &[(usize, usize)]) -> Vec<i16> {
    bursts
        .iter()
        .flat_map(|&(speech, quiet)| {
            std::iter::repeat_n(2000, speech).chain(std::iter::repeat_n(0, quiet))
        })
        .collect()
}

fn replay(
    samples: &[i16],
    chunk: usize,
    fail_every: usize,
    cue: bool,
) -> (Vec<u8>, Vec<u8>, Option<Partition>, usize) {
    let mut pcm = Pcm16::with_reservation(samples.len(), samples.len());
    let mut speech = Audibility::new(1000);
    let mut cuts = Cuts::new(1000);
    let mut pieces = Vec::new();
    let mut attempted = 0_usize;
    let mut minimum = usize::MAX;
    for chunk in samples.chunks(chunk) {
        for &sample in chunk {
            pcm.push(sample);
        }
        if cue {
            speech.expect_cue(pcm.audio());
        }
        speech.extend(pcm.audio());
        if let Some(range) = cuts.offer(&speech, pcm.audio().len()) {
            attempted = attempted.saturating_add(1);
            if fail_every == 0 || !attempted.is_multiple_of(fail_every) {
                minimum = minimum.min(range.len());
                pieces.extend_from_slice(&pcm.audio()[range.clone()]);
                cuts.sent(range.end);
            }
        }
    }
    speech.complete(pcm.audio());
    let partition = cuts.finish(&speech, pcm.audio().len());
    let sealed = pcm.seal(speech, 1000).unwrap().unwrap();
    if let Some(partition) = &partition {
        pieces.extend_from_slice(&sealed.wav[44_usize.saturating_add(partition.tail.start)..]);
    }
    (sealed.wav.clone(), pieces, partition, minimum)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn accepted_pieces_and_tail_equal_the_unchanged_final_pcm(
        bursts in prop::collection::vec((1000_usize..30_000,0_usize..2000),1..7),
        chunk in 1_usize..400, failures in 0_usize..5, cue in any::<bool>(),
    ) {
        let samples = audio(&bursts);
        let (wav,pieces,partition,minimum) = replay(&samples,chunk,failures,cue);
        let mut baseline = Pcm16::with_reservation(samples.len(),samples.len());
        for sample in samples { baseline.push(sample); }
        let mut gate = Audibility::new(1000);
        if cue { gate.expect_cue(baseline.audio()); }
        let sealed = baseline.seal(gate,1000).unwrap().unwrap();
        prop_assert_eq!(&wav,&sealed.wav);
        if partition.is_some() {
            prop_assert_eq!(pieces,&wav[44..]);
            prop_assert!(minimum >= 40_000);
        }
    }
}

#[test]
fn short_recordings_do_not_cut_and_small_audible_tails_remain_in_the_tail() {
    let short = audio(&[(19_000, 1100)]);
    let (_, pieces, partition, _) = replay(&short, 20, 0, false);
    assert_eq!(pieces, Vec::<u8>::new());
    assert_eq!(partition, None);
    let long = audio(&[(21_000, 1100)]);
    let (_, _, partition, _) = replay(&long, 20, 0, false);
    assert_eq!(partition.map(|part| part.pieces), Some(1));
    let resumed = audio(&[(21_000, 1100), (20, 1100)]);
    let (wav, pieces, partition, _) = replay(&resumed, 20, 0, false);
    let tail = partition.unwrap().tail;
    assert_eq!(pieces, &wav[44..]);
    assert_eq!(tail.len(), 2240);
    assert!(
        wav.len() > 44 + 43_000,
        "The short final word still belongs to the recorded audio"
    );
}
