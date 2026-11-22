use super::*;

#[test]
#[ignore = "Opt in: native encoder/decoder with synthetic resizing frames; no capture or input"]
fn irregular_captures_never_appear_before_their_capture_time() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("resizing.mp4");
    let mut encoder = Encoder::open(&path, canvas::Canvas::new([320, 240])?, FILE_LIMIT)?;
    let captures = [
        (0_u64, 40_u8),
        (33, 60),
        (67, 80),
        (101, 100),
        (437, 120),
        (753, 140),
        (1121, 160),
        (1999, 180),
        (2501, 200),
        (2700, 220),
    ];
    for (index, &(ms, shade)) in captures.iter().enumerate() {
        let [width, height] = match index % 3 {
            0 => [320, 240],
            1 => [160, 240],
            _ => [320, 120],
        };
        assert!(
            encoder
                .write(
                    &frame(width, height, shade),
                    Duration::from_millis(ms),
                    &mut || false,
                )?
                .is_continue()
        );
    }
    assert!(
        encoder
            .advance(End::Before(Duration::from_secs(3)), &mut || false)?
            .is_continue()
    );
    encoder.finish()?;
    let samples = sample_times(&path)?;
    assert_eq!(samples.len(), 90, "Three seconds at 30 fps");
    for (ticks, _) in samples {
        let at = Duration::from_nanos(u64::try_from(ticks)?.saturating_mul(100));
        let decoded = decode(&path, [320, 240], at)?;
        let shade = decoded.image.pixels()[(120 * 320 + 160) * 4];
        let (source, _) = captures
            .iter()
            .enumerate()
            .min_by_key(|(_, (_, written))| shade.abs_diff(*written))
            .unwrap();
        let expected = captures
            .partition_point(|&(ms, _)| Duration::from_millis(ms) <= decoded.at)
            .saturating_sub(1);
        assert_eq!(
            source, expected,
            "Presentation at {ticks} used a future or stale captured image"
        );
    }
    Ok(())
}

#[test]
#[ignore = "Opt in: native encoder/decoder with synthetic pixels; no capture or input"]
fn a_quiet_screen_saves_a_readable_fragment_before_finalization() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("quiet.mp4");
    let mut encoder = Encoder::open(&path, canvas::Canvas::new([320, 240])?, FILE_LIMIT)?;
    assert!(
        encoder
            .write(&frame(320, 240, 80), Duration::ZERO, &mut || false)?
            .is_continue()
    );
    for second in 1..=3 {
        assert!(
            encoder
                .advance(End::Through(Duration::from_secs(second)), &mut || false)?
                .is_continue()
        );
    }
    let coverage = encoder.coverage().unwrap();
    assert_eq!(coverage.last, Duration::ZERO);
    assert_eq!(coverage.frames, 91);
    let interrupted = directory.path().join("before-finalize.mp4");
    let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
    loop {
        std::fs::copy(&path, &interrupted)?;
        if sample_times(&interrupted).is_ok_and(|samples| samples.len() >= 30) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Quiet capture did not save a fragment before finalization"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let decoded = decode(&interrupted, [320, 240], Duration::from_millis(500))?;
    assert!(decoded.image.pixels()[0].abs_diff(80) <= 8);
    encoder.finish()?;
    Ok(())
}
