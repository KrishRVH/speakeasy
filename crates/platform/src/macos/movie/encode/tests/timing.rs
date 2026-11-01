use super::*;
use crate::{
    Part,
    movie::{
        canvas::Canvas,
        encode::{Encoder, End, Observer},
    },
};

#[derive(Default)]
struct Geometry(Vec<(u32, [u32; 4], Part)>);

impl Observer for Geometry {
    fn cancelled(&mut self) -> bool {
        false
    }

    fn geometry(&mut self, sample: u32, content: [u32; 4], part: Part) -> anyhow::Result<bool> {
        self.0.push((sample, content, part));
        Ok(true)
    }
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "Fourteen synthetic samples and fixed 320 by 240 pixel coordinates fit every calculation"
)]
pub(super) fn verify(directory: &Path) -> anyhow::Result<()> {
    let path = directory.join("timing.mp4");
    let mut encoder = Encoder::<Codec>::open(&path, Canvas::new([320, 240])?, 384 << 20)?;
    let mut geometry = Geometry::default();
    let patterns = [0, 1].map(|index| crate::movie::tests::color_frame(index, [320, 240]));
    for (at, index) in [(0, 0), (40, 1), (67, 0)] {
        assert!(
            encoder
                .write(&patterns[index], Duration::from_millis(at), &mut geometry)?
                .is_continue()
        );
    }
    let part = Part::between([0.2, 0.1], [0.7, 0.6]);
    let resized = crate::Frame {
        at: Instant::now(),
        width: 160,
        height: 120,
        bgra: [90, 140, 200, 255].repeat(160 * 120),
        part,
    };
    assert!(
        encoder
            .write(&resized, Duration::from_millis(200), &mut geometry)?
            .is_continue()
    );
    assert!(
        encoder
            .advance(End::Through(Duration::from_millis(450)), &mut geometry)?
            .is_continue()
    );
    let coverage = encoder.coverage().unwrap();
    assert_eq!(coverage.frames, 14);
    assert_eq!(coverage.last, Duration::from_millis(200));
    assert_eq!(coverage.at, Duration::from_nanos(433_333_333));
    assert_eq!(
        geometry.0,
        [
            (0, [0, 0, 320, 240], Part::WHOLE),
            (6, [80, 60, 160, 120], part)
        ]
    );
    encoder.finish()?;
    let prefix = inspect(&path)?;
    assert_eq!(prefix.frames, 14);
    assert!(!prefix.truncated);
    for (index, sample) in prefix.samples().iter().enumerate() {
        let expected = Duration::from_nanos(u64::try_from(index)? * 1_000_000_000 / 30);
        assert_eq!(sample.at, expected);
        let frame =
            crate::read_movie_frame(File::open(&path)?, &prefix, expected, || false)?.unwrap();
        assert_eq!(frame.at, expected);
        if index < 6 {
            crate::movie::tests::check_colors(&frame, usize::from(index == 2));
        } else {
            for (x, y, expected) in [
                (10, 10, [0, 0, 0]),
                (310, 230, [0, 0, 0]),
                (160, 120, [90, 140, 200]),
            ] {
                let offset = (y * 320 + x) * 4;
                assert!(
                    frame.bgra[offset..offset + 3]
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| actual.abs_diff(expected) < 12)
                );
            }
        }
    }
    Ok(())
}
