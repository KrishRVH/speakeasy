use super::*;

#[test]
#[ignore = "Opt in: native encoder/decoder with synthetic resizing frames; choose SPEAKEASY_MOVIE_PROBE_DIRECTORY"]
fn irregular_captures_never_appear_before_their_capture_time() -> anyhow::Result<()> {
    use std::io::Write as _;

    let directory = probe_directory()?;
    let path = directory.join("resizing.mp4");
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
    let mut log = std::fs::File::create_new(directory.join("resizing.csv"))?;
    writeln!(log, "pts100ns,duration100ns,source,sourceMs")?;
    for (ticks, duration) in samples {
        let at = Duration::from_nanos(u64::try_from(ticks)?.saturating_mul(100));
        let decoded = decode(&path, [320, 240], at)?;
        let shade = decoded.image.pixels()[(120 * 320 + 160) * 4];
        let (source, &(captured_ms, _)) = captures
            .iter()
            .enumerate()
            .min_by_key(|(_, (_, written))| shade.abs_diff(*written))
            .unwrap();
        writeln!(log, "{ticks},{duration},{source},{captured_ms}")?;
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
fn pending_captures_reach_the_video_only_as_the_newest_at_a_later_tick() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    for (name, through, read, presented, shown) in [
        ("stopped.mp4", None, 32, 0, 40),
        ("coalesced.mp4", Some(34), 34, 33_333_300, 120),
    ] {
        let path = directory.path().join(name);
        let mut encoder = Encoder::open(&path, canvas::Canvas::new([320, 240])?, FILE_LIMIT)?;
        for (ms, shade) in [(0, 40), (31, 80), (32, 120)] {
            assert!(
                encoder
                    .write(
                        &frame(320, 240, shade),
                        Duration::from_millis(ms),
                        &mut || false
                    )?
                    .is_continue()
            );
        }
        if let Some(through) = through {
            assert!(
                encoder
                    .advance(End::Through(Duration::from_millis(through)), &mut || false)?
                    .is_continue()
            );
        }
        encoder.finish()?;
        let decoded = decode(&path, [320, 240], Duration::from_millis(read))?;
        assert_eq!(decoded.at.as_nanos(), presented, "{name}");
        assert!(
            decoded.image.pixels()[0].abs_diff(shown) <= 8,
            "An unpresented or superseded image reached {name}"
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

#[test]
#[ignore = "Opt in: native encoder with synthetic pixels and deterministic cancellation"]
fn cancelled_catchup_keeps_its_committed_output_without_backfilling_on_close() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cancelled.mp4");
    let mut encoder = Encoder::open(&path, canvas::Canvas::new([320, 240])?, FILE_LIMIT)?;
    assert!(
        encoder
            .write(&frame(320, 240, 50), Duration::ZERO, &mut || false)?
            .is_continue()
    );
    let mut checks = 0_u32;
    let result = encoder.advance(End::Through(Duration::from_secs(1)), &mut || {
        let count = checks;
        checks = checks.saturating_add(1);
        count == 3
    })?;
    assert!(result.is_break());
    let coverage = encoder.coverage().unwrap();
    assert_eq!(coverage.frames, 4);
    assert_eq!(coverage.last, Duration::ZERO);
    assert_eq!(coverage.at, Duration::from_millis(100));
    encoder.finish()?;
    assert_eq!(sample_times(&path)?.len(), 4);
    Ok(())
}

#[tokio::test]
#[ignore = "Opt in: native encoder and synthetic pixels; no capture or input"]
async fn a_stalled_writer_reports_the_held_prefix_even_without_a_new_capture() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("stalled.mp4");
    let (control, target) = crate::MovieControl::new(
        path.clone(),
        Instant::now()
            .checked_add(Duration::from_secs(300))
            .unwrap(),
        Duration::MAX,
    );
    let (_stop, stopping) = async_channel::bounded(1);
    let native = crate::OwnedThread::spawn("synthetic movie stall", move || {
        let image = frame(320, 240, 80);
        let at = image.at;
        let mut movie = Movie::new(Some(target));
        assert!(movie.frame(&image, &stopping).is_ok());
        assert!(
            movie
                .poll(at.checked_add(Duration::from_secs(3)).unwrap(), &stopping)
                .is_err()
        );
        assert!(!movie.wants_frames());
    })?;
    let Some(crate::MovieEvent::Started { at, accepted, .. }) = control.next().await else {
        anyhow::bail!("Synthetic movie did not request its origin");
    };
    assert!(accepted.send(true).is_ok());
    let Some(crate::MovieEvent::Geometry {
        sample, accepted, ..
    }) = control.next().await
    else {
        anyhow::bail!("Synthetic movie did not request its content rectangle");
    };
    assert_eq!(sample, 0);
    assert!(accepted.send(true).is_ok());
    native.join().await?;
    let Some(crate::MovieEvent::Finished {
        last,
        through,
        frames,
        finalized,
    }) = control.next().await
    else {
        anyhow::bail!("Synthetic movie did not finish");
    };
    assert_eq!(last, at);
    assert_eq!(through.duration_since(at), Duration::from_secs(2));
    assert_eq!(frames, 61);
    assert!(!finalized);
    assert!(control.next().await.is_none());
    assert_eq!(sample_times(&path)?.len(), 61);
    Ok(())
}
