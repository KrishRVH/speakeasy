use std::io::Write as _;

use super::*;

#[derive(Default)]
struct Receipts(Vec<(u32, [u32; 4], crate::Part)>);

impl crate::movie::encode::Observer for Receipts {
    fn cancelled(&mut self) -> bool {
        false
    }

    fn geometry(
        &mut self,
        sample: u32,
        content: [u32; 4],
        part: crate::Part,
    ) -> anyhow::Result<bool> {
        self.0.push((sample, content, part));
        Ok(true)
    }
}

#[test]
#[ignore = "Opt in: native encoder/decoder with synthetic resizing pixels; no capture or input"]
fn receipts_describe_presented_layouts_and_exclude_superseded_inputs() -> anyhow::Result<()> {
    let directory = probe_directory()?;
    let path = directory.join("geometry.mp4");
    let mut encoder = Encoder::open(&path, canvas::Canvas::new([320, 240])?, FILE_LIMIT)?;
    let mut receipts = Receipts::default();
    for (ms, width, height) in [
        (0, 320, 240),
        (31, 160, 240),
        (32, 320, 120),
        (100, 160, 240),
        (400, 160, 240),
    ] {
        let mut image = frame(width, height, 150);
        image.part = if width < 320 {
            Part {
                left: 0.25,
                top: 0.0,
                right: 0.75,
                bottom: 1.0,
            }
        } else if height < 240 {
            Part {
                left: 0.0,
                top: 0.25,
                right: 1.0,
                bottom: 0.75,
            }
        } else {
            Part::WHOLE
        };
        if ms == 400 {
            image.part.left = 0.0;
            image.part.right = 0.5;
        }
        assert!(
            encoder
                .write(&image, Duration::from_millis(ms), &mut receipts)?
                .is_continue()
        );
    }
    assert!(
        encoder
            .advance(End::Before(Duration::from_secs(1)), &mut receipts)?
            .is_continue()
    );
    encoder.finish()?;
    assert_eq!(
        receipts
            .0
            .iter()
            .map(|&(sample, content, _)| (sample, content))
            .collect::<Vec<_>>(),
        [
            (0, [0, 0, 320, 240]),
            (1, [0, 60, 320, 120]),
            (3, [80, 0, 160, 240]),
            (12, [80, 0, 160, 240])
        ]
    );
    let mut csv = std::fs::File::create_new(directory.join("geometry.csv"))?;
    for &(sample, [left, top, width, height], part) in &receipts.0 {
        writeln!(
            csv,
            "{sample},{left},{top},{width},{height},{},{},{},{}",
            part.left, part.top, part.right, part.bottom
        )?;
    }
    for (index, (ticks, _)) in sample_times(&path)?.into_iter().enumerate() {
        let at = Duration::from_nanos(u64::try_from(ticks)? * 100);
        let frame = decode(&path, [320, 240], at)?;
        let [left, top, width, height] = receipts
            .0
            .iter()
            .rev()
            .find(|(sample, _, _)| usize::try_from(*sample).unwrap() <= index)
            .unwrap()
            .1;
        for (offset, pixel) in frame.image.pixels().as_chunks::<4>().0.iter().enumerate() {
            let x = u32::try_from(offset % 320)?;
            let y = u32::try_from(offset / 320)?;
            let inside = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            assert!(
                pixel[0].abs_diff(if inside { 150 } else { 0 }) <= 8,
                "Decoded content disagrees with its saved rectangle at sample {index}"
            );
        }
    }
    Ok(())
}
