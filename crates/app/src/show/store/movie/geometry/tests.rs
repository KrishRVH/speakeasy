use std::{fs, time::Duration};

use proptest::prelude::*;

use super::*;

const ID: [u8; 16] = [7; 16];
const WHOLE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const SIZE: [u32; 2] = [320, 240];
const CHANGES: [(u32, [u32; 4]); 3] = [
    (0, [0, 0, 320, 240]),
    (30, [80, 0, 160, 240]),
    (60, [0, 60, 320, 120]),
];

fn journal(directory: &Path) -> anyhow::Result<Vec<u8>> {
    let mut writing = Writing::create(directory, ID, SIZE)?;
    for (sample, content) in CHANGES {
        writing.record(sample, content, WHOLE)?;
    }
    Ok(fs::read(directory.join(FILE))?)
}

#[test]
fn every_torn_record_bounds_geometry_without_losing_playable_video() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let bytes = journal(directory.path())?;
    for length in 0..=bytes.len() {
        fs::write(directory.path().join(FILE), &bytes[..length])?;
        let kept = recover(directory.path(), ID, SIZE, 90)?;
        let records = length.saturating_sub(HEADER_BYTES) / RECORD_BYTES;
        let through = if records == 0 {
            0
        } else if (length - HEADER_BYTES).is_multiple_of(RECORD_BYTES) {
            90
        } else {
            CHANGES[records - 1].0 + 1
        };
        assert_eq!(kept.through, through, "tear at byte {length}");
        for sample in 0..90 {
            let expected = (sample < through).then(|| {
                CHANGES[..records]
                    .iter()
                    .rev()
                    .find(|(start, _)| *start <= sample)
                    .unwrap()
                    .1
            });
            assert_eq!(
                kept.at(sample).map(|change| change.content),
                expected,
                "sample {sample}, tear {length}"
            );
        }
    }
    Ok(())
}

#[test]
fn corrupt_receipts_stop_at_their_last_known_sample_and_future_receipts_bound_the_prefix()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let bytes = journal(directory.path())?;
    for offset in 0..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[offset] ^= 1;
        fs::write(directory.path().join(FILE), &damaged)?;
        let kept = recover(directory.path(), ID, SIZE, 90)?;
        let good = offset.saturating_sub(HEADER_BYTES) / RECORD_BYTES;
        let expected = if offset < HEADER_BYTES || good == 0 {
            0
        } else {
            CHANGES[good - 1].0 + 1
        };
        assert_eq!(kept.through, expected, "corruption at {offset}");
    }
    let mut damaged = bytes.clone();
    damaged.push(0);
    fs::write(directory.path().join(FILE), damaged)?;
    let kept = recover(directory.path(), ID, SIZE, 20)?;
    assert_eq!(kept.through, 20);
    assert_eq!(kept.changes.len(), 1);
    assert_eq!(kept.at(19).map(|change| change.content), Some(CHANGES[0].1));
    fs::write(directory.path().join(FILE), bytes)?;
    assert_eq!(recover(directory.path(), ID, SIZE, 20)?.through, 20);
    assert_eq!(recover(directory.path(), [8; 16], SIZE, 90)?.through, 0);
    assert_eq!(recover(directory.path(), ID, [640, 480], 90)?.through, 0);
    Ok(())
}

proptest! {
    #[test]
    fn rectangles_cannot_escape_even_with_overflowing_edges(content in any::<[u32; 4]>()) {
        let [left, top, width, height] = content;
        let inside = width > 0 && height > 0
            && u64::from(left).saturating_add(u64::from(width)) <= u64::from(SIZE[0])
            && u64::from(top).saturating_add(u64::from(height)) <= u64::from(SIZE[1]);
        prop_assert_eq!(validate(Change { sample: 0, content, part: WHOLE }, None, SIZE).is_ok(), inside);
    }
}

#[test]
fn receipt_order_and_coverage_must_match_the_video() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut writing = Writing::create(directory.path(), ID, SIZE)?;
    assert!(writing.record(1, CHANGES[0].1, WHOLE).is_err());
    writing.record(0, CHANGES[0].1, WHOLE)?;
    for (sample, content) in [
        (0, CHANGES[1].1),
        (1, CHANGES[0].1),
        (9000, CHANGES[1].1),
        (1, [0, 0, 0, 1]),
    ] {
        assert!(writing.record(sample, content, WHOLE).is_err());
    }
    writing.record(1, CHANGES[1].1, WHOLE)?;
    let mut geometry = recover(directory.path(), ID, SIZE, 2)?;
    assert_eq!(geometry.changes.len(), 2);
    geometry.through = 1;
    assert!(geometry.validate(SIZE, 2).is_err());
    geometry.through = 3;
    assert!(geometry.validate(SIZE, 2).is_err());
    geometry.through = 2;
    geometry.changes.remove(0);
    assert!(geometry.validate(SIZE, 2).is_err());
    Ok(())
}

#[test]
fn crops_use_the_decoded_sample_and_expand_inside_content_instead_of_letterboxing()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    super::super::tests::checkpoint(directory.path(), false)?;
    let path = directory.path().join(super::super::FILE);
    let mut bytes = fs::read(&path)?;
    let mdhd = bytes.windows(4).position(|bytes| bytes == b"mdhd").unwrap();
    bytes[mdhd + 16..mdhd + 20].copy_from_slice(&30_000_u32.to_be_bytes());
    fs::write(path, bytes)?;
    let mut movie = super::super::inspect(directory.path())?.unwrap();
    movie.metadata.size = SIZE;
    movie.prefix.size = SIZE;
    movie.metadata.geometry = Some(Geometry {
        through: 2,
        changes: vec![
            Change {
                sample: 0,
                content: [0, 0, 320, 240],
                part: WHOLE,
            },
            Change {
                sample: 1,
                content: [80, 0, 160, 240],
                part: [0.25, 0.0, 0.75, 1.0],
            },
        ],
    });
    assert_eq!(movie.prefix.samples()[1], Duration::from_nanos(33_333_333));
    let pixels: Vec<_> = (0..240)
        .flat_map(|_| (0..320).flat_map(|x| [u8::try_from(x % 256).unwrap(), 50, 90, 255]))
        .collect();
    let frame = |at| speakeasy_platform::MovieFrame {
        at,
        size: SIZE,
        bgra: pixels.clone(),
    };
    let bounds = || {
        Some(super::super::Crop {
            bounds: Rect::from([0.0, 0.0, 0.0, 0.0]),
            part: Rect::from([0.25, 0.0, 0.75, 1.0]),
        })
    };
    let (cropped, rect) = crop(&movie, frame(Duration::from_nanos(33_333_300)), bounds())?;
    assert_eq!(
        rect,
        Some(PixelRect {
            left: 80,
            top: 0,
            width: 32,
            height: 48
        })
    );
    let expected = Image::from_bgra(320, 240, pixels.clone())?.cropped(80, 0, 32, 48)?;
    assert_eq!(cropped.encode_png()?, expected.encode_png()?);
    assert!(crop(&movie, frame(Duration::ZERO), bounds()).is_err());
    assert!(crop(&movie, frame(Duration::from_nanos(33_333_333)), bounds()).is_err());
    movie.metadata.geometry.as_mut().unwrap().through = 1;
    assert!(crop(&movie, frame(Duration::from_nanos(33_333_300)), bounds()).is_err());
    movie.metadata.geometry = None;
    assert!(crop(&movie, frame(Duration::ZERO), bounds()).is_err());
    let (raw, rect) = crop(&movie, frame(Duration::ZERO), None)?;
    assert!(rect.is_none());
    assert_eq!([raw.width(), raw.height()], SIZE);
    Ok(())
}
