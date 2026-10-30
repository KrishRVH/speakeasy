//! Synthetic PCM exercises prefix recovery and the allocation limits before any engine is used.

use proptest::prelude::*;

use super::*;

fn recorded(samples: &[u8]) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    for record in [
        Record::Begin {
            rate: 8_000,
            start: Duration::from_millis(125),
            language: "en".into(),
        },
        Record::Cue { before: 0, at: 0 },
        Record::Samples(Zeroizing::new(samples.to_vec())),
        Record::End,
    ] {
        bytes.extend_from_slice(&record.encode().unwrap());
    }
    bytes
}

proptest! {
    #[test]
    fn tears_and_single_bit_damage_never_invent_audio(
        samples in proptest::collection::vec(any::<i16>(), 1..512),
        cut in any::<usize>(),
        bit in 0_u8..8,
    ) {
        let pcm: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        let bytes = recorded(&pcm);
        let at = cut.checked_rem(bytes.len()).unwrap();
        if let Ok(prefix) = decode(&bytes[..at], at) {
            prop_assert!(pcm.starts_with(&prefix.pcm));
            prop_assert!(!prefix.complete);
        }
        let mut damaged = bytes.clone();
        damaged[at] ^= 1 << bit;
        if let Ok(prefix) = decode(damaged.as_slice(), damaged.len()) {
            prop_assert!(pcm.starts_with(&prefix.pcm));
        }
        let audio = decode(bytes.as_slice(), bytes.len()).unwrap();
        prop_assert_eq!(&*audio.pcm, &pcm);
        prop_assert_eq!(audio.cues, vec![(0, 0)]);
        prop_assert_eq!(audio.start, Duration::from_millis(125));
        prop_assert_eq!(audio.language.as_ref(), "en");
        prop_assert!(audio.complete);
    }
}

#[test]
fn an_oversized_record_stops_at_the_valid_prefix_before_allocating_it() {
    let mut bytes = recorded(&[1, 0, 2, 0]);
    bytes.truncate(bytes.len() - Record::End.encode().unwrap().len());
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    let audio = decode(bytes.as_slice(), bytes.len()).unwrap();
    assert_eq!(&*audio.pcm, &[1, 0, 2, 0]);
    assert!(!audio.complete);
}
