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

#[test]
fn replay_matches_live_thinning_geometry_and_pointer_fold() {
    let colors = [50_u8, 250, 110, 111, 0, 190, 75, 125, 255];
    let budget = 1_000;
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
    assert!(replayed.ended());
    let (recording, journal) = live.finish();
    let (restored, held) = replayed.finish();
    assert_eq!(recording, restored);
    assert_eq!(journal.bytes(), held.bytes());
    assert!(journal.samples().len() < colors.len());
    assert_eq!(journal.samples().len(), held.samples().len());
    for (before, after) in journal.samples().iter().zip(held.samples()) {
        assert_eq!(before.at, after.at);
        assert_eq!(&before.signature, &after.signature);
        assert_eq!(journal.frame_at(before.at), held.frame_at(before.at));
    }
}

#[test]
fn a_torn_ending_recovers_the_frames_with_a_screen_stopped_notice() {
    let mut live = Take::new(1_000, surface(), Framing::Automatic);
    let mut bytes = header(1_000, surface(), Framing::Automatic).unwrap();
    let image = arrival(&mut live, 100, gray(42));
    bytes.extend(encode(&image).unwrap());
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

    let torn = replay(&bytes[..bytes.len() - 1]).unwrap();
    assert!(!torn.ended());
    assert_eq!(
        torn.journal().frame_at(Moment::from_millis(100)),
        Some(gray(42))
    );
    assert_eq!(torn.finish().0.ending, Ending::ScreenStopped);
    let finished = replay(&bytes).unwrap();
    assert!(finished.ended());
    assert_eq!(finished.finish().0.ending, Ending::Finished);
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
fn a_checkpoint_restores_window_layouts_and_chosen_framing() {
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
    let mut bytes = header(0, surface(), Framing::Chosen).unwrap();
    apply(&mut live, &TakeEvent::Windows(first.clone()), &mut bytes);
    apply(&mut live, &TakeEvent::Windows(second.clone()), &mut bytes);
    let restored = replay(&bytes).unwrap();
    assert_eq!(restored.framing(), Framing::Chosen);
    assert_eq!(restored.journal().layouts(), [first, second]);
}
