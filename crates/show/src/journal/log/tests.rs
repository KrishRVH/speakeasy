use proptest::{prelude::*, sample::Index};

use super::*;
use crate::journal::tests::{damaged, incompressible};

mod gaps;

fn surface() -> Size {
    Size {
        width: 800.0,
        height: 600.0,
    }
}

fn apply(take: &mut Take, event: &TakeEvent, bytes: &mut Vec<u8>) {
    take.apply(event).unwrap();
    bytes.extend(encode(event).unwrap());
}

fn gray(shade: u8) -> Image {
    Image::filled(2, 2, [shade; 3]).unwrap()
}

fn arrival(take: &mut Take, at: u64, image: Image) -> TakeEvent {
    take.arrive(Moment::from_millis(at), || Some(image))
        .unwrap()
}

fn replay(bytes: &[u8]) -> Result<Take, SourceError> {
    let header = bytes
        .get(..HEADER_BYTES)
        .ok_or(SourceError("missing header"))?;
    let mut take = start(header)?;
    let mut rest = bytes.get(HEADER_BYTES..).unwrap_or_default();
    while let Some(prefix) = rest.get(..4) {
        let Ok(length) = record_size(prefix) else {
            break;
        };
        let Some((record, tail)) = rest.split_at_checked(length) else {
            break;
        };
        let Ok(event) = decode(record) else {
            break;
        };
        if take.apply(&event).is_err() {
            break;
        }
        rest = tail;
    }
    Ok(take)
}

fn every_kind_of_event() -> Vec<TakeEvent> {
    let mut take = Take::new(1_000, surface(), Framing::Automatic);
    let mut arrive = |at, shade| {
        let image = Image::filled(70, 66, [shade; 3]).unwrap();
        arrival(&mut take, at, image)
    };
    let (whole, changed) = (arrive(100, 40), arrive(200, 80));
    vec![
        TakeEvent::Surface(surface()),
        TakeEvent::Pointer {
            at: Moment::from_millis(150),
            point: Point::new(0.4, 0.6),
            part: Rect::from([0.0, 0.0, 1.0, 1.0]),
            drawing: true,
            pressed: false,
        },
        TakeEvent::PointerGap(Moment::from_millis(160)),
        TakeEvent::Windows(Layout {
            at: Moment::from_millis(170),
            windows: vec![Window {
                id: 1,
                app: 2,
                layer: Layer::Floating,
                bounds: Rect::from([0.1, 0.1, 0.6, 0.7]),
            }],
        }),
        TakeEvent::Pin(Span::new(
            Moment::from_millis(180),
            Moment::from_millis(190),
        )),
        whole,
        changed,
        TakeEvent::End {
            at: Moment::from_millis(300),
            ending: Ending::WindowClosed,
        },
    ]
}

proptest! {
    #[test]
    fn replay_matches_live_thinning_geometry_and_pointer_fold(
        colors in prop::collection::vec(any::<u8>(), 1..35),
        budget in 0..2_000_usize,
    ) {
        let mut live = Take::new(budget, surface(), Framing::Automatic);
        let mut bytes = header(budget, surface(), Framing::Automatic).unwrap();
        for (index, color) in colors.into_iter().enumerate() {
            let at = Moment::from_millis(u64::try_from(index).unwrap().saturating_mul(100));
            let shade = f32::from(color);
            let resized = Size {
                width: 800.0 + shade,
                height: 600.0,
            };
            apply(&mut live, &TakeEvent::Surface(resized), &mut bytes);
            let width = 65 + u32::from(color % 2);
            let event = live
                .arrive(at, || Some(Image::filled(width, 66, [color; 3]).unwrap()))
                .unwrap();
            bytes.extend(encode(&event).unwrap());
            let pointer = TakeEvent::Pointer {
                part: Rect::from([0.0, 0.0, 0.5 + shade / 510.0, 1.0]),
                at,
                point: Point::new(shade / 255.0, 0.5),
                drawing: index % 3 != 0,
                pressed: false,
            };
            apply(&mut live, &pointer, &mut bytes);
        }
        let end = TakeEvent::End {
            at: Moment::from_millis(4_000),
            ending: Ending::WindowClosed,
        };
        apply(&mut live, &end, &mut bytes);
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
    fn damage_inside_a_well_framed_event_is_decoded_or_refused_without_panicking(
        chosen in any::<Index>(),
        at in any::<Index>(),
        byte in any::<u8>(),
        keep in prop::option::of(any::<Index>()),
    ) {
        let events = every_kind_of_event();
        let position = chosen.index(events.len());
        let record = encode(&events[position]).unwrap();
        if let Ok(event) = decode(&damaged(&record, at, byte, keep)) {
            let mut take = Take::new(1_000, surface(), Framing::Automatic);
            for earlier in &events[..position] {
                take.apply(earlier).unwrap();
            }
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Replay may accept or refuse the damaged event; the property only rejects panics"
            )]
            let _ = take.apply(&event);
        }
    }

    #[test]
    fn arbitrary_records_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..20_000)) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The property only requires decoding to return; either outcome passes"
        )]
        let _ = decode(&bytes);
    }
}

#[test]
fn every_torn_tail_recovers_only_whole_events_and_keeps_an_honest_ending() {
    let mut live = Take::new(1_000, surface(), Framing::Automatic);
    let mut bytes = header(1_000, surface(), Framing::Automatic).unwrap();
    let image = arrival(&mut live, 100, gray(42));
    bytes.extend(encode(&image).unwrap());
    let first_frame_end = bytes.len();
    apply(
        &mut live,
        &TakeEvent::Pointer {
            part: Rect::from([0.0, 0.0, 1.0, 1.0]),
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
        if end < HEADER_BYTES {
            assert!(result.is_err());
            continue;
        }
        let take = result.unwrap();
        assert!(!take.ended());
        assert_eq!(!take.journal().is_empty(), end >= first_frame_end);
        assert_eq!(take.finish().0.ending, Ending::ScreenStopped);
    }
    assert!(replay(&bytes).unwrap().ended());
    bytes[..8].copy_from_slice(b"SPKLOG02");
    assert!(replay(&bytes).is_err());
}

#[test]
fn a_checkpoint_keeps_who_frames_the_recording() {
    for framing in [Framing::Automatic, Framing::Chosen] {
        let bytes = header(0, surface(), framing).unwrap();
        assert_eq!(start(&bytes).unwrap().framing(), framing);
    }
}

#[test]
fn a_changed_tile_frame_needs_the_frame_it_changes() {
    let mut live = Take::new(0, surface(), Framing::Automatic);
    arrival(&mut live, 0, gray(0));
    let changed = arrival(&mut live, 100, gray(1));
    let mut orphan = Take::new(0, surface(), Framing::Automatic);
    assert!(orphan.apply(&changed).is_err());
    assert!(orphan.journal().is_empty());
}

#[test]
fn an_image_the_sampling_stride_skips_is_refused() {
    let mut take = Take::new(0, surface(), Framing::Automatic);
    for (at, shade) in [(0, 0), (100, 1), (200, 2)] {
        arrival(&mut take, at, gray(shade));
    }
    let held = take.journal().samples().len();
    let admitted = arrival(
        &mut Take::new(100, surface(), Framing::Automatic),
        400,
        gray(1),
    );
    assert!(
        take.apply(&admitted).is_err(),
        "Thinning halved admission, so the fourth arrival is skipped"
    );
    assert_eq!(take.journal().samples().len(), held);
}

#[test]
fn a_surface_without_a_positive_finite_size_is_refused() {
    let mut take = Take::new(0, surface(), Framing::Automatic);
    for width in [f32::NAN, 0.0, -1.0, f32::INFINITY] {
        let surface = TakeEvent::Surface(Size { width, height: 1.0 });
        assert!(take.apply(&surface).is_err(), "{width}");
    }
}

#[test]
fn a_record_with_a_failed_checksum_or_an_impossible_length_is_refused() {
    let event = arrival(&mut Take::new(0, surface(), Framing::Automatic), 0, gray(1));
    let mut corrupt = encode(&event).unwrap();
    let last_payload_byte = corrupt.len() - size_of::<u32>() - 1;
    corrupt[last_payload_byte] ^= 1;
    assert!(decode(&corrupt).is_err());
    assert!(record_size(&u32::MAX.to_le_bytes()).is_err());
}

#[test]
fn debugging_an_arrival_never_prints_its_pixels() {
    let shown = |shade| {
        let mut take = Take::new(100, surface(), Framing::Automatic);
        format!("{:?}", arrival(&mut take, 0, gray(shade)))
    };
    assert_eq!(shown(0), shown(255));
}

#[test]
fn arrivals_after_end_or_the_time_limit_never_prepare_pixels() {
    let mut take = Take::new(1_000, surface(), Framing::Automatic);
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
    let image = incompressible();
    let mut live = Take::new(1 << 20, surface(), Framing::Automatic);
    let encoded = encode(&arrival(&mut live, 0, image.clone())).unwrap();
    let mut replayed = Take::new(1 << 20, surface(), Framing::Automatic);
    replayed.apply(&decode(&encoded).unwrap()).unwrap();
    assert_eq!(replayed.journal().frame_at(Moment::START), Some(image));
}

#[test]
fn encoding_rejects_a_tile_that_a_reader_cannot_accept() {
    let mut take = Take::new(1_000, surface(), Framing::Automatic);
    let mut event = arrival(&mut take, 0, gray(0));
    let TakeEvent::Arrival {
        entry: Some(entry), ..
    } = &mut event
    else {
        panic!("The first image is admitted");
    };
    entry.tiles[0].1 = vec![0; 4_096].into();
    assert!(encode(&event).is_err());
}

#[test]
fn a_checkpoint_restores_window_layouts_and_refuses_impossible_ones() {
    let window = |id: u32, layer, bounds: [f32; 4]| Window {
        id,
        app: id % 2,
        layer,
        bounds: Rect::from(bounds),
    };
    let first = Layout {
        at: Moment::from_millis(100),
        windows: vec![
            window(1, Layer::Menu, [0.1, 0.0, 0.3, 0.4]),
            window(2, Layer::Normal, [0.1, 0.1, 0.6, 0.7]),
            window(3, Layer::Bar, [0.0, 0.0, 1.0, 0.03]),
        ],
    };
    let second = Layout {
        at: Moment::from_millis(300),
        windows: vec![
            window(4, Layer::Floating, [0.8, 0.05, 0.95, 0.2]),
            window(2, Layer::Normal, [0.2, 0.1, 0.7, 0.7]),
        ],
    };
    let mut live = Take::new(0, surface(), Framing::Automatic);
    let mut bytes = header(0, surface(), Framing::Automatic).unwrap();
    apply(&mut live, &TakeEvent::Windows(first.clone()), &mut bytes);
    apply(&mut live, &TakeEvent::Windows(second.clone()), &mut bytes);
    let restored = [first.clone(), second.clone()];
    assert_eq!(replay(&bytes).unwrap().journal().layouts(), restored);

    let earlier = Layout {
        at: Moment::from_millis(200),
        ..first
    };
    assert!(live.apply(&TakeEvent::Windows(earlier)).is_err());
    let outside = Layout {
        at: Moment::from_millis(400),
        windows: vec![window(4, Layer::Normal, [0.5, 0.5, 1.2, 0.9])],
    };
    assert!(live.apply(&TakeEvent::Windows(outside.clone())).is_err());
    assert!(encode(&TakeEvent::Windows(outside)).is_err());
    let crowded = Layout {
        windows: vec![window(5, Layer::Normal, [0.1, 0.1, 0.2, 0.2]); MOST_WINDOWS + 1],
        ..second
    };
    assert!(encode(&TakeEvent::Windows(crowded)).is_err());
    let mut unknown = 500_u64.to_le_bytes().to_vec();
    unknown.extend([1_u32, 6, 0].into_iter().flat_map(u32::to_le_bytes));
    unknown.push(4);
    unknown.extend(
        [0.1_f32, 0.1, 0.2, 0.2]
            .into_iter()
            .flat_map(f32::to_le_bytes),
    );
    assert!(decode(&framed(WINDOWS, &unknown).unwrap()).is_err());
    assert_eq!(live.finish().1.layouts(), restored);
}
