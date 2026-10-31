//! Source evidence must survive thinning and size changes, reject damage, and load only one frame.

use proptest::prelude::*;

use super::*;

fn encode(journal: &Journal, duration: Moment) -> Vec<u8> {
    let mut bytes = Vec::new();
    journal
        .write_source::<SourceError>(duration, |chunk| {
            bytes.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    bytes
}

fn read(bytes: &[u8], range: Range<u64>) -> Result<Vec<u8>, SourceError> {
    let start = usize::try_from(range.start).unwrap();
    let end = usize::try_from(range.end).unwrap();
    bytes
        .get(start..end)
        .map(<[u8]>::to_vec)
        .ok_or(SourceError("fixture range is missing"))
}

fn open(bytes: &[u8]) -> Result<Source, SourceError> {
    Source::read(u64::try_from(bytes.len()).unwrap(), |range| {
        read(bytes, range)
    })
}

fn fixture() -> (Journal, Moment) {
    let mut journal = Journal::with_budget(1_000);
    for n in 0..24_u8 {
        if let Some(frame) = journal.admit() {
            frame.record(
                Moment::from_millis(u64::from(n) * 200),
                Image::filled(67 + u32::from(n % 2), 65, [n.saturating_mul(10), 0, 0]).unwrap(),
            );
        }
    }
    (journal, Moment::from_millis(5_000))
}

#[test]
fn source_preserves_retained_frames_after_thinning_and_geometry_changes() {
    let (journal, duration) = fixture();
    let bytes = encode(&journal, duration);
    let source = open(&bytes).unwrap();
    assert!(source.stride() > 1);
    assert_eq!(source.arrivals(), 24);
    assert_eq!(
        source.moments(),
        journal.samples().iter().map(|s| s.at).collect::<Vec<_>>()
    );
    for sample in journal.samples() {
        let served = source
            .frame(sample.at, |range| read(&bytes, range))
            .unwrap();
        assert_eq!(served.at, sample.at);
        assert_eq!(served.image, journal.frame_at(sample.at).unwrap());
    }
    let between = Moment::from_millis(4_999);
    let served = source.frame(between, |range| read(&bytes, range)).unwrap();
    assert!(
        served.at < between,
        "The served timestamp must not pretend to be the request"
    );
    assert_eq!(served.image, journal.frame_at(between).unwrap());
    assert!(
        source
            .frame(
                duration.saturating_add(std::time::Duration::from_millis(1)),
                |range| read(&bytes, range)
            )
            .is_err()
    );
}

#[test]
fn indexing_never_reads_pixels_and_a_frame_never_loads_the_entire_source() {
    let mut journal = Journal::new();
    for n in 0..4_u8 {
        journal.admit().unwrap().record(
            Moment::from_millis(u64::from(n) * 200),
            Image::filled(65, 65, [n * 50, 0, 0]).unwrap(),
        );
    }
    let bytes = encode(&journal, Moment::from_millis(1_000));
    let mut ranges = Vec::new();
    let source = Source::read(u64::try_from(bytes.len()).unwrap(), |range| {
        ranges.push(range.clone());
        read(&bytes, range)
    })
    .unwrap();
    assert_eq!(ranges.len(), 3);
    assert_eq!(ranges[0], 0..8);
    assert!(
        ranges[2].start > 8,
        "Reading metadata decoded or scanned source pixels"
    );
    let mut fetched = 0;
    let served = source
        .frame(Moment::START, |range| {
            fetched += range.end - range.start;
            assert!(
                (range.start >= 8 && range.end <= ranges[2].start)
                    || range == (ranges[2].end.saturating_sub(4)..ranges[2].end)
            );
            read(&bytes, range)
        })
        .unwrap();
    assert_eq!(served.image, journal.frame_at(Moment::START).unwrap());
    assert!(fetched < ranges[2].start - 8);
}

#[test]
fn torn_sources_and_damaged_tiles_never_produce_partial_images() {
    let (journal, duration) = fixture();
    let bytes = encode(&journal, duration);
    for end in 0..bytes.len() {
        assert!(open(&bytes[..end]).is_err());
    }
    let source = open(&bytes).unwrap();
    let mut damaged = bytes.clone();
    damaged[13] ^= 0x40;
    assert!(
        source
            .frame(Moment::START, |range| read(&damaged, range))
            .is_err()
    );
    let mut damaged = bytes;
    let end = damaged.len();
    damaged[end - 17] ^= 1;
    assert!(open(&damaged).is_err());
}

#[test]
fn short_but_validly_compressed_tiles_cannot_fill_missing_pixels_with_black() {
    let tile = super::super::Tile {
        left: 0,
        top: 0,
        width: 2,
        height: 2,
    };
    let short = Version {
        first: 0,
        data: fdeflate::compress_to_vec(&[0; 4]).into(),
    };
    assert!(short.restore(tile, 8, &mut [0; 16]).is_none());
}

proptest! {
    #[test]
    fn generated_geometry_and_budgets_round_trip(
        colors in prop::collection::vec((any::<[u8; 3]>(), 1..100_u32, 1..100_u32), 1..30),
        budget in 1..8_000_usize,
    ) {
        let mut journal = Journal::with_budget(budget);
        for (n, (color, width, height)) in colors.into_iter().enumerate() {
            if let Some(frame) = journal.admit() {
                frame.record(Moment::from_millis(u64::try_from(n).unwrap().saturating_mul(200)),
                    Image::filled(width, height, color).unwrap());
            }
        }
        let bytes = encode(&journal, Moment::from_millis(6_000));
        let source = open(&bytes).unwrap();
        for sample in journal.samples() {
            let frame = source.frame(sample.at, |range| read(&bytes, range)).unwrap();
            prop_assert_eq!(frame.image, journal.frame_at(sample.at).unwrap());
        }
    }

    #[test]
    fn arbitrary_indexes_are_bounded_and_never_reach_tile_reads(
        bytes in prop::collection::vec(any::<u8>(), 0..4_096),
    ) {
        let mut archive = MAGIC.to_vec();
        emit_record::<SourceError>(INDEX_RECORD, &bytes, &mut |chunk| {
            archive.extend_from_slice(chunk);
            Ok(())
        }).unwrap();
        archive.extend_from_slice(&u64::try_from(bytes.len().saturating_add(RECORD_OVERHEAD)).unwrap().to_le_bytes());
        archive.extend_from_slice(FOOTER);
        prop_assert!(open(&archive).is_err());
    }
}

#[test]
fn replacing_a_source_with_same_sized_evidence_cannot_reuse_an_old_timestamp() {
    let source_at = |at| {
        let mut journal = Journal::new();
        journal.admit().unwrap().record(
            Moment::from_millis(at),
            Image::filled(2, 2, [9; 3]).unwrap(),
        );
        encode(&journal, Moment::from_millis(1_000))
    };
    let old = source_at(100);
    let new = source_at(200);
    assert_eq!(old.len(), new.len());
    assert!(
        open(&old)
            .unwrap()
            .frame(Moment::START, |range| read(&new, range))
            .is_err()
    );
}

#[test]
fn high_entropy_tiles_remain_readable_when_deflate_expands_the_pixels() {
    let image = Image::from_bgra(
        64,
        64,
        (0..4_096)
            .flat_map(|n| {
                if n % 2 == 0 {
                    [0, 0, 0, 255]
                } else {
                    [128, 128, 128, 255]
                }
            })
            .collect(),
    )
    .unwrap();
    let mut journal = Journal::new();
    journal
        .admit()
        .unwrap()
        .record(Moment::START, image.clone());
    let bytes = encode(&journal, Moment::from_millis(100));
    assert_eq!(
        open(&bytes)
            .unwrap()
            .frame(Moment::START, |range| read(&bytes, range))
            .unwrap()
            .image,
        image
    );
}

#[test]
fn writing_rejects_a_tile_that_a_reader_cannot_accept() {
    let mut journal = Journal::new();
    journal
        .admit()
        .unwrap()
        .record(Moment::START, Image::from_bgra(2, 2, vec![0; 16]).unwrap());
    journal.segments[0].tiles[0][0].data = vec![0; 4_096].into();
    assert!(
        journal
            .write_source::<SourceError>(Moment::from_millis(100), |_| Ok(()))
            .is_err()
    );
}

#[test]
fn crop_geometry_checks_intermediate_samples_without_reading_pixels() {
    let mut journal = Journal::new();
    for (at, width) in [(100, 100), (1000, 200), (2000, 100)] {
        journal.admit().unwrap().record(
            Moment::from_millis(at),
            Image::filled(width, 100, [1, 2, 3]).unwrap(),
        );
    }
    let source = open(&encode(&journal, Moment::from_millis(3000))).unwrap();
    let span = |start, end| Span::new(Moment::from_millis(start), Moment::from_millis(end));
    assert!(source.stable_geometry(span(0, 999)));
    assert!(!source.stable_geometry(span(500, 2500)));
    assert!(
        !source.stable_geometry(span(1500, 2500)),
        "The carried-in frame is a different size"
    );
    assert!(source.stable_geometry(span(2000, 3000)));
}
