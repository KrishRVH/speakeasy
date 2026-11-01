use super::{fixture, fixture::padded, inspect};

#[test]
fn normalization_preserves_samples_and_all_bytes_except_the_declared_header_flag() {
    let (original, flag) = padded(fixture::movie().bytes, 0, &[0; 4], None);
    let prefix = inspect(&original).unwrap();
    let mut expected = original.clone();
    expected[flag] = 17;
    for width in 1..=original.len() {
        let mut bytes = original.clone();
        for (index, chunk) in bytes.chunks_mut(width).enumerate() {
            prefix
                .compatibility()
                .apply(u64::try_from(index * width).unwrap(), chunk)
                .unwrap();
        }
        assert_eq!(bytes, expected);
        prefix.compatibility().apply(0, &mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }
    let normalized = inspect(&expected).unwrap();
    assert_eq!(normalized.samples(), prefix.samples());
    assert_eq!(normalized.duration, prefix.duration);
    assert_eq!(normalized.bytes, prefix.bytes);
    assert_eq!(normalized.compatibility().headers, [0_u64; 0]);
    let mut changed = original;
    changed[flag] = 33;
    assert!(prefix.compatibility().apply(0, &mut changed).is_err());
    assert!(prefix.compatibility().apply(u64::MAX, &mut [0]).is_err());
}

#[test]
fn normalization_never_reinterprets_optional_fields_or_inherited_sizes() {
    for (padding, flags) in [(&[0; 4][..], Some(0x0101_0000)), (&[0; 3][..], None)] {
        let (original, _) = padded(fixture::movie().bytes, 0, padding, flags);
        let prefix = inspect(&original).unwrap();
        let mut bytes = original.clone();
        prefix.compatibility().apply(0, &mut bytes).unwrap();
        assert_eq!(bytes, original);
    }
    let mut bytes = fixture::movie().bytes;
    let defaults = bytes.windows(4).position(|tag| tag == b"trex").unwrap();
    bytes[defaults + 20..defaults + 24].copy_from_slice(&6_u32.to_be_bytes());
    let run = bytes.windows(4).position(|tag| tag == b"trun").unwrap();
    bytes[run + 4..run + 8].copy_from_slice(&0x0100_0D01_u32.to_be_bytes());
    for kind in [b"moof", b"traf", b"trun"] {
        let tag = bytes.windows(4).position(|tag| tag == kind).unwrap();
        let size = u32::from_be_bytes(bytes[tag - 4..tag].try_into().unwrap());
        bytes[tag - 4..tag].copy_from_slice(&(size - 4).to_be_bytes());
    }
    bytes.drain(run + 20..run + 24);
    let (original, _) = padded(bytes, 0, &[0; 4], None);
    let prefix = inspect(&original).unwrap();
    let mut bytes = original.clone();
    prefix.compatibility().apply(0, &mut bytes).unwrap();
    assert_eq!(bytes, original);
}

#[test]
fn rejected_fragments_never_contribute_header_corrections() {
    let (mut bytes, _) = padded(fixture::movie().bytes, 1, &[0; 4], None);
    bytes.truncate(bytes.len() - 1);
    let prefix = inspect(&bytes).unwrap();
    assert_eq!(prefix.frames, 1);
    assert!(prefix.truncated);
    let mut corrected = bytes.clone();
    prefix.compatibility().apply(0, &mut corrected).unwrap();
    assert_eq!(corrected, bytes);
}

proptest::proptest! {
    #[test]
    fn arbitrary_ranges_only_change_the_intersecting_header(start in 0_usize..1200, length in 0_usize..1200) {
        let (original, flag) = padded(fixture::movie().bytes, 0, &[0; 4], None);
        let prefix = inspect(&original).unwrap();
        let start = start.min(original.len());
        let end = start.saturating_add(length).min(original.len());
        let mut part = original[start..end].to_vec();
        prefix.compatibility().apply(u64::try_from(start).unwrap(), &mut part).unwrap();
        let mut expected = original;
        expected[flag] = 17;
        proptest::prop_assert_eq!(part, &expected[start..end]);
    }
}
