use super::*;

mod fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/movie.rs"
    ));
}

fn inspect(bytes: &[u8]) -> Result<MoviePrefix, MovieError> {
    MoviePrefix::read(u64::try_from(bytes.len()).unwrap(), |range| {
        Ok(bytes
            .get(usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap())
            .unwrap()
            .to_vec())
    })
}

#[test]
fn sample_navigation_preserves_fractional_times_instead_of_assuming_a_frame_rate() {
    let mut bytes = fixture::movie().bytes;
    let media = bytes.windows(4).position(|bytes| bytes == b"mdhd").unwrap();
    bytes[media + 16..media + 20].copy_from_slice(&30_000_u32.to_be_bytes());
    let prefix = inspect(&bytes).unwrap();
    assert_eq!(
        prefix.samples(),
        [Duration::ZERO, Duration::from_nanos(33_333_333)]
    );
    assert_eq!(prefix.duration, Duration::from_nanos(66_666_666));
    let run = bytes.windows(4).position(|bytes| bytes == b"trun").unwrap();
    bytes[run + 16..run + 20].copy_from_slice(&1_500_u32.to_be_bytes());
    assert_eq!(
        inspect(&bytes).unwrap().samples(),
        [Duration::ZERO, Duration::from_millis(50)]
    );
}

#[test]
fn recovery_never_claims_samples_from_an_incomplete_fragment() {
    let fixture = fixture::movie();
    let complete = inspect(&fixture.bytes).unwrap();
    assert_eq!(complete.frames, 2);
    assert_eq!(complete.duration, Duration::from_secs(2));
    assert_eq!(complete.samples(), [Duration::ZERO, Duration::from_secs(1)]);
    assert!(!complete.truncated);
    for cut in fixture.initialized..fixture.bytes.len() {
        let result = inspect(&fixture.bytes[..cut]);
        if cut < fixture.ends[0] {
            assert!(result.is_err());
        } else {
            let prefix = result.unwrap();
            assert_eq!(prefix.bytes, u64::try_from(fixture.ends[0]).unwrap());
            assert_eq!(prefix.frames, 1);
            assert_eq!(prefix.samples(), [Duration::ZERO]);
            assert_eq!(prefix.duration, Duration::from_secs(1));
            assert_eq!(prefix.truncated, cut != fixture.ends[0]);
        }
    }
}

#[test]
fn misplaced_sample_bytes_and_hidden_audio_cannot_pass_as_video() {
    let fixture = fixture::movie();
    let mut bytes = fixture.bytes.clone();
    let trun = bytes.windows(4).position(|bytes| bytes == b"trun").unwrap();
    bytes[trun + 12..trun + 16].copy_from_slice(&0_u32.to_be_bytes());
    assert!(inspect(&bytes).is_err());
    let mut bytes = fixture.bytes;
    let handler = bytes.windows(4).position(|bytes| bytes == b"vide").unwrap();
    bytes[handler..handler + 4].copy_from_slice(b"soun");
    assert!(inspect(&bytes).is_err());
}

#[test]
fn damaged_second_sample_preserves_only_the_first_fragment() {
    let fixture = fixture::movie();
    let mut bytes = fixture.bytes;
    let trun = bytes
        .windows(4)
        .rposition(|bytes| bytes == b"trun")
        .unwrap();
    bytes[trun + 20..trun + 24].copy_from_slice(&7_u32.to_be_bytes());
    let prefix = inspect(&bytes).unwrap();
    assert_eq!(prefix.frames, 1);
    assert!(prefix.truncated);
    assert_eq!(prefix.bytes, u64::try_from(fixture.ends[0]).unwrap());
}

#[test]
fn implicit_sample_offsets_must_point_at_the_data_payload() {
    let mut bytes = fixture::movie().bytes;
    let moof = bytes.windows(4).position(|bytes| bytes == b"moof").unwrap();
    let traf = bytes.windows(4).position(|bytes| bytes == b"traf").unwrap();
    let trun = bytes.windows(4).position(|bytes| bytes == b"trun").unwrap();
    bytes[trun + 4..trun + 8].copy_from_slice(&0x0100_0F00_u32.to_be_bytes());
    for at in [moof, traf, trun] {
        let size = u32::from_be_bytes(bytes[at - 4..at].try_into().unwrap());
        bytes[at - 4..at].copy_from_slice(&(size - 4).to_be_bytes());
    }
    bytes.drain(trun + 12..trun + 16);
    assert!(
        inspect(&bytes).is_err(),
        "Implicit run addressed the moof instead of image data"
    );
}

#[test]
fn codec_parameter_lengths_cannot_escape_the_configuration() {
    let mut bytes = fixture::movie().bytes;
    let avc = bytes.windows(4).position(|bytes| bytes == b"avcC").unwrap() + 4;
    bytes[avc + 6..avc + 8].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(
        inspect(&bytes).is_err(),
        "Truncated parameter bytes passed initialization"
    );
}

#[test]
fn presentation_offsets_cannot_shift_the_acknowledged_movie_origin() {
    let mut bytes = fixture::movie().bytes;
    let runs: Vec<_> = bytes
        .windows(4)
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == b"trun").then_some(index))
        .collect();
    for run in runs {
        bytes[run + 28..run + 32].copy_from_slice(&1_000_u32.to_be_bytes());
    }
    assert!(
        inspect(&bytes).is_err(),
        "Presentation began after the saved session origin"
    );
}

proptest::proptest! {
    #[test]
    fn arbitrary_sample_lengths_never_escape_the_data_box(size in proptest::num::u32::ANY) {
        let fixture=fixture::movie();
        let mut bytes=fixture.bytes;
        let at=bytes.windows(4).position(|bytes|bytes==b"trun").unwrap().saturating_add(20);
        bytes.get_mut(at..at.saturating_add(4)).unwrap().copy_from_slice(&size.to_be_bytes());
        proptest::prop_assert_eq!(inspect(&bytes).is_ok(), size==6);
    }
}
