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
        Record::Samples(samples.to_vec()),
        Record::End,
    ] {
        bytes.extend_from_slice(&record.encode().unwrap());
    }
    bytes
}

proptest! {
    #[test]
    fn a_torn_audio_log_recovers_only_its_written_prefix(
        samples in proptest::collection::vec(any::<i16>(), 1..512),
        cut in any::<usize>(),
    ) {
        let pcm: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        let bytes = recorded(&pcm);
        let at = cut.checked_rem(bytes.len()).unwrap();
        if let Some(prefix) = decode(&bytes[..at]).unwrap() {
            prop_assert!(pcm.starts_with(&prefix.pcm));
            prop_assert!(!prefix.complete);
        }
        let audio = decode(bytes.as_slice()).unwrap().unwrap();
        prop_assert_eq!(&audio.pcm, &pcm);
        prop_assert_eq!(audio.cues, vec![(0, 0)]);
        prop_assert_eq!(audio.rate.get(), 8_000);
        prop_assert_eq!(audio.start, Duration::from_millis(125));
        prop_assert_eq!(audio.language.as_ref(), "en");
        prop_assert!(audio.complete);
    }
}
