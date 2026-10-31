//! Live/replay equivalence, valid prefixes after tears, and corrupt count/geometry trust boundaries.

use proptest::prelude::*;

use super::*;

mod gaps;

fn surface() -> Size {
    Size {
        width: 800.0,
        height: 600.0,
    }
}

fn apply(take: &mut Take, event: &TakeEvent, bytes: &mut Vec<u8>) {
    take.apply(event).unwrap();
    bytes.extend(Log::encode(event).unwrap());
}

fn replay(bytes: &[u8]) -> Result<Take, SourceError> {
    let header = bytes
        .get(..Log::HEADER_BYTES)
        .ok_or(SourceError("missing header"))?;
    let mut take = Log::start(header)?;
    let mut rest = bytes.get(Log::HEADER_BYTES..).unwrap_or_default();
    while let Some(prefix) = rest.get(..4) {
        let Ok(length) = Log::record_size(prefix) else {
            break;
        };
        let Some((record, tail)) = rest.split_at_checked(length) else {
            break;
        };
        let Ok(event) = Log::decode(record) else {
            break;
        };
        if take.apply(&event).is_err() {
            break;
        }
        rest = tail;
    }
    Ok(take)
}

proptest! {
    #[test]
    fn replay_matches_live_thinning_geometry_and_pointer_fold(colors in prop::collection::vec(any::<u8>(), 1..35), budget in 0..2_000_usize) {
        let mut live = Take::new(budget, surface());
        let mut bytes = Log::header(budget, surface()).unwrap();
        for (index, color) in colors.into_iter().enumerate() {
            let at = Moment::from_millis(u64::try_from(index).unwrap().saturating_mul(100));
            apply(&mut live, &TakeEvent::Surface(Size { width: 800.0 + f32::from(color), height: 600.0 }), &mut bytes);
            let event = live.arrive(at, || Some(Image::filled(65 + u32::from(color % 2), 66, [color; 3]).unwrap())).unwrap();
            bytes.extend(Log::encode(&event).unwrap());
            apply(&mut live, &TakeEvent::Pointer { part: (index % 3 != 0).then(|| Rect::from([0.0, 0.0, 0.5 + f32::from(color) / 510.0, 1.0])), at, point: Point::new(f32::from(color) / 255.0, 0.5), drawing: index % 3 != 0, pressed: false }, &mut bytes);
        }
        apply(&mut live, &TakeEvent::End { at: Moment::from_millis(4_000), ending: Ending::WindowClosed }, &mut bytes);
        let replayed = replay(&bytes).unwrap();
        prop_assert!(replayed.ended());
        let (recording, journal) = live.finish();
        let (restored, held) = replayed.finish();
        prop_assert_eq!(recording, restored);
        prop_assert_eq!(journal.bytes(), held.bytes());
        prop_assert_eq!(journal.samples().len(), held.samples().len());
        for (before, after) in journal.samples().iter().zip(held.samples()) {
            prop_assert_eq!(before.at, after.at);
            prop_assert_eq!(&before.signature, &after.signature);
            prop_assert_eq!(journal.frame_at(before.at), held.frame_at(before.at));
        }
    }

    #[test]
    fn arbitrary_records_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..20_000)) {
        let _result = Log::decode(&bytes);
    }
}

#[test]
fn every_torn_tail_recovers_only_whole_events_and_keeps_an_honest_ending() {
    let mut live = Take::new(1_000, surface());
    let mut bytes = Log::header(1_000, surface()).unwrap();
    let image = live
        .arrive(Moment::from_millis(100), || {
            Some(Image::filled(2, 2, [42; 3]).unwrap())
        })
        .unwrap();
    bytes.extend(Log::encode(&image).unwrap());
    let first_frame_end = bytes.len();
    apply(
        &mut live,
        &TakeEvent::Pointer {
            part: None,
            at: Moment::from_millis(200),
            point: Point::new(0.4, 0.6),
            drawing: false,
            pressed: true,
        },
        &mut bytes,
    );
    apply(
        &mut live,
        &TakeEvent::End {
            at: Moment::from_millis(500),
            ending: Ending::Finished,
        },
        &mut bytes,
    );
    for end in 0..bytes.len() {
        let result = replay(&bytes[..end]);
        if end < Log::HEADER_BYTES {
            assert!(result.is_err());
            continue;
        }
        let take = result.unwrap();
        assert!(!take.ended());
        assert_eq!(!take.journal().is_empty(), end >= first_frame_end);
        assert_eq!(take.finish().0.ending, Ending::ScreenStopped);
    }
    assert!(replay(&bytes).unwrap().ended());
}

#[test]
fn malformed_events_cannot_change_the_good_prefix_or_print_pixels() {
    let mut take = Take::new(0, surface());
    let mut bytes = Log::header(0, surface()).unwrap();
    for n in 0..3_u64 {
        let event = take
            .arrive(Moment::from_millis(n * 100), || {
                Some(Image::filled(2, 2, [u8::try_from(n).unwrap(); 3]).unwrap())
            })
            .unwrap();
        bytes.extend(Log::encode(&event).unwrap());
    }
    assert_eq!(
        replay(&bytes).unwrap().journal().samples().len(),
        take.journal().samples().len()
    );
    let before = take.journal().samples().len();
    let mut fresh = Take::new(100, surface());
    let event = fresh
        .arrive(Moment::from_millis(400), || {
            Some(Image::filled(2, 2, [1; 3]).unwrap())
        })
        .unwrap();
    assert!(
        take.apply(&event).is_err(),
        "The stride should reject this admitted image"
    );
    assert_eq!(take.journal().samples().len(), before);
    assert!(
        take.apply(&TakeEvent::Surface(Size {
            width: f32::NAN,
            height: 1.0
        }))
        .is_err()
    );
    assert!(Log::record_size(&u32::MAX.to_le_bytes()).is_err());
    let mut corrupt = Log::encode(&event).unwrap();
    corrupt[20] ^= 0xFF;
    assert!(Log::decode(&corrupt).is_err());
    assert!(!format!("{event:?}").contains("signature"));
}

#[test]
fn arrivals_after_end_or_the_time_limit_never_prepare_pixels() {
    let mut take = Take::new(1_000, surface());
    assert!(
        take.arrive(Moment::from_millis(360_001), || panic!(
            "Invalid arrival prepared pixels"
        ))
        .is_err()
    );
    take.apply(&TakeEvent::End {
        at: Moment::START,
        ending: Ending::Finished,
    })
    .unwrap();
    assert!(
        take.arrive(Moment::START, || panic!("Ended take prepared pixels"))
            .is_err()
    );
}

#[test]
fn high_entropy_tiles_round_trip_even_when_compression_expands_them() {
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
    let mut live = Take::new(1 << 20, surface());
    let event = live.arrive(Moment::START, || Some(image.clone())).unwrap();
    let encoded = Log::encode(&event).unwrap();
    let mut replayed = Take::new(1 << 20, surface());
    replayed.apply(&Log::decode(&encoded).unwrap()).unwrap();
    assert_eq!(replayed.journal().frame_at(Moment::START), Some(image));
}

#[test]
fn encoding_rejects_a_tile_that_a_reader_cannot_accept() {
    let mut take = Take::new(1_000, surface());
    let mut event = take
        .arrive(Moment::START, || {
            Some(Image::from_bgra(2, 2, vec![0; 16]).unwrap())
        })
        .unwrap();
    let TakeEvent::Arrival {
        entry: Some(entry), ..
    } = &mut event
    else {
        panic!("The first image is admitted");
    };
    entry.tiles[0].1 = vec![0; 4_096].into();
    assert!(Log::encode(&event).is_err());
}
