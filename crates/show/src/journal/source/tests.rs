use std::time::Duration;

use super::*;
use crate::{Ending, Framing, Size, Take, TakeEvent, journal::tests::incompressible};

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
            .frame(duration.saturating_add(Duration::from_millis(1)), |range| {
                read(&bytes, range)
            })
            .is_err()
    );
}

#[test]
fn indexing_never_reads_pixels_and_a_frame_never_loads_the_entire_source() {
    let mut journal = Journal::default();
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
    let [magic, _footer, index]: [Range<u64>; 3] = ranges.try_into().unwrap();
    assert_eq!(magic, 0..MAGIC.len() as u64);
    assert!(
        index.start > magic.end,
        "Reading metadata decoded or scanned source pixels"
    );
    let mut fetched = 0;
    let served = source
        .frame(Moment::START, |range| {
            fetched += range.end - range.start;
            assert!(range.start >= magic.end && range.end <= index.start);
            read(&bytes, range)
        })
        .unwrap();
    assert_eq!(served.image, journal.frame_at(Moment::START).unwrap());
    assert!(fetched < index.start - magic.end);
}

#[test]
fn torn_sources_and_damaged_tiles_never_produce_partial_images() {
    let (journal, duration) = fixture();
    let bytes = encode(&journal, duration);
    assert!(open(&bytes[..bytes.len() - 1]).is_err());
    let source = open(&bytes).unwrap();
    let mut damaged = bytes;
    let first_tile_payload = MAGIC.len() + size_of::<u32>() + size_of::<u8>();
    damaged[first_tile_payload] ^= 0x40;
    assert!(
        source
            .frame(Moment::START, |range| read(&damaged, range))
            .is_err()
    );
}

#[test]
fn tiles_that_deflate_expands_remain_readable() {
    let image = incompressible();
    let mut journal = Journal::default();
    journal
        .admit()
        .unwrap()
        .record(Moment::START, image.clone());
    assert!(
        journal.segments[0].tiles[0][0].data.len() > 64 * 64 * PIXEL,
        "Deflate no longer expands the tile"
    );
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
fn crop_geometry_checks_intermediate_samples_without_reading_pixels() {
    let mut journal = Journal::default();
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

#[test]
fn arrivals_at_the_time_limit_end_after_their_last_frame_with_a_readable_source() {
    let take = || {
        Take::new(
            1 << 20,
            Size {
                width: 20.0,
                height: 20.0,
            },
            Framing::Chosen,
        )
    };
    let (mut live, mut replay) = (take(), take());
    let last_moment = DURATION_LIMIT.saturating_sub(Duration::from_millis(1));
    let mut saved = None;
    for value in [0, 100, 200] {
        let arrival = live
            .arrive(last_moment, || Image::filled(20, 20, [value; 3]).ok())
            .unwrap();
        replay.apply(&arrival).unwrap();
        if let TakeEvent::Arrival {
            entry: Some(entry), ..
        } = arrival
        {
            saved = Some(entry);
        }
    }
    let unsampled = TakeEvent::Arrival {
        at: last_moment,
        entry: saved,
    };
    assert!(
        replay.apply(&unsampled).is_err(),
        "Replay accepted a frame at the time limit, where live sampling records none"
    );
    let early = TakeEvent::End {
        at: last_moment,
        ending: Ending::Finished,
    };
    assert!(
        replay.apply(&early).is_err(),
        "Replay accepted an ending before the last saved frame"
    );
    let ending = live.end(last_moment, Ending::Finished);
    assert!(
        matches!(ending, TakeEvent::End { at, .. } if at == DURATION_LIMIT),
        "{ending:?}"
    );
    replay.apply(&ending).unwrap();
    for take in [live, replay] {
        let (recording, journal) = take.finish();
        assert_eq!(recording.duration, DURATION_LIMIT);
        let source = open(&encode(&journal, recording.duration)).unwrap();
        assert_eq!(source.moments(), [last_moment, DURATION_LIMIT]);
    }
}
