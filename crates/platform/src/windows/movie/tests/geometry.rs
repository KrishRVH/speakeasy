use std::io::Write as _;

use super::*;

#[derive(Default)]
struct Receipts(Vec<(u32, [u32; 4], crate::Part)>);

impl encoder::Observer for Receipts {
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
fn content_rectangles_equal_painted_pixels_after_odd_and_tiny_resizes() -> anyhow::Result<()> {
    let mut canvas = canvas::Canvas::new([8, 6])?;
    for [width, height] in [[8, 6], [3, 5], [7, 3], [1, 1], [13, 17], [2560, 1]] {
        canvas.paint(&frame(width, height, 150))?;
        let [left, top, width, height] = canvas.content();
        assert!(width > 0 && height > 0);
        for (index, pixel) in canvas.held().as_chunks::<4>().0.iter().enumerate() {
            let x = u32::try_from(index % 8)?;
            let y = u32::try_from(index / 8)?;
            let inside = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            assert_eq!(*pixel, if inside { [150, 150, 150, 255] } else { [0; 4] });
        }
    }
    Ok(())
}

#[test]
#[ignore = "Opt in: native encoder/decoder with synthetic resizing pixels; no capture or input"]
fn receipts_describe_presented_layouts_and_exclude_superseded_inputs() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var_os("SPEAKEASY_MOVIE_PROBE_DIRECTORY")
            .ok_or_else(|| anyhow::anyhow!("Choose a synthetic probe directory"))?,
    );
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
    for (index, (ticks, _)) in encoder::sample_times(&path)?.into_iter().enumerate() {
        let at = Duration::from_nanos(u64::try_from(ticks)? * 100);
        let frame = read::frame(std::fs::File::open(&path)?, [320, 240], at, &|| false)?.unwrap();
        let [left, top, width, height] = receipts
            .0
            .iter()
            .rev()
            .find(|(sample, _, _)| usize::try_from(*sample).unwrap() <= index)
            .unwrap()
            .1;
        for (offset, pixel) in frame.bgra.as_chunks::<4>().0.iter().enumerate() {
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

#[tokio::test]
#[ignore = "Opt in: native encoder with synthetic pixels and blocked geometry acknowledgements"]
async fn rejected_and_cancelled_geometry_never_submit_the_unacknowledged_layout()
-> anyhow::Result<()> {
    for rejected in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("interrupted.mp4");
        let (control, target) = crate::MovieControl::new(
            path.clone(),
            Instant::now().checked_add(crate::PATIENCE).unwrap(),
        );
        let (_stop, stopping) = async_channel::bounded(1);
        let native = crate::OwnedThread::spawn("synthetic geometry acceptance", move || {
            let first = frame(320, 240, 150);
            let mut movie = Movie::new(Some(target));
            assert!(movie.frame(&first, &stopping));
            let mut resized = frame(160, 240, 200);
            resized.at = first.at.checked_add(Duration::from_millis(100)).unwrap();
            assert_eq!(movie.frame(&resized, &stopping), !rejected);
            assert!(!movie.active());
        })?;
        let Some(crate::MovieEvent::Started { accepted, .. }) = control.next().await else {
            anyhow::bail!("Missing synthetic origin");
        };
        accepted.send(true).await?;
        let Some(crate::MovieEvent::Geometry {
            sample: 0,
            accepted,
            ..
        }) = control.next().await
        else {
            anyhow::bail!("Missing synthetic first rectangle");
        };
        accepted.send(true).await?;
        let Some(crate::MovieEvent::Geometry {
            sample: 3,
            accepted,
            ..
        }) = control.next().await
        else {
            anyhow::bail!("Missing synthetic resize rectangle");
        };
        if rejected {
            accepted.send(false).await?;
        } else {
            control.request_stop();
        }
        native.join().await;
        let Some(crate::MovieEvent::Finished {
            frames, finalized, ..
        }) = control.next().await
        else {
            anyhow::bail!("Missing synthetic completion");
        };
        assert_eq!(frames, 3);
        assert_eq!(finalized, !rejected);
        assert!(control.next().await.is_none());
        assert_eq!(encoder::sample_times(&path)?.len(), 3);
        std::fs::rename(path, directory.path().join("closed.mp4"))?;
    }
    Ok(())
}
