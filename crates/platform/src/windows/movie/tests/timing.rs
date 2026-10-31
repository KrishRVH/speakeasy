use super::*;

#[test]
#[ignore = "Opt in: native encoder/decoder with synthetic resizing frames; choose SPEAKEASY_MOVIE_PROBE_DIRECTORY"]
fn irregular_captures_never_appear_before_their_capture_time() -> anyhow::Result<()> {
    use std::io::Write as _;

    let directory = PathBuf::from(
        std::env::var_os("SPEAKEASY_MOVIE_PROBE_DIRECTORY")
            .ok_or_else(|| anyhow::anyhow!("Choose a synthetic probe directory"))?,
    );
    let path = directory.join("resizing.mp4");
    let mut encoder = Encoder::open(&path, canvas::Canvas::new([320, 240])?, FILE_LIMIT)?;
    let inputs = [0_u64, 33, 67, 101, 437, 753, 1121, 1999, 2501, 2700];
    for (index, at) in inputs.iter().copied().enumerate() {
        let [width, height] = match index % 3 {
            0 => [320, 240],
            1 => [160, 240],
            _ => [320, 120],
        };
        assert!(
            encoder
                .write(
                    &frame(width, height, 40 + u8::try_from(index)? * 20),
                    Duration::from_millis(at),
                    &|| false,
                )?
                .is_continue()
        );
    }
    assert!(
        encoder
            .advance(End::Before(Duration::from_secs(3)), &|| false)?
            .is_continue()
    );
    encoder.finish()?;
    let bytes = path.metadata()?.len();
    let mut log = std::fs::File::create_new(directory.join("resizing.csv"))?;
    writeln!(log, "pts100ns,duration100ns,source,sourceMs")?;
    for (ticks, duration) in encoder::sample_times(&path)? {
        let at = Duration::from_nanos(u64::try_from(ticks)?.saturating_mul(100));
        let decoded = read::frame(std::fs::File::open(&path)?, [320, 240], at, &|| false)?
            .ok_or_else(|| anyhow::anyhow!("Synthetic read was cancelled"))?;
        let shade = decoded.bgra[(120 * 320 + 160) * 4];
        let source = inputs
            .iter()
            .enumerate()
            .min_by_key(|(index, _)| shade.abs_diff(40 + u8::try_from(*index).unwrap() * 20))
            .map(|(index, at)| (index, *at))
            .unwrap();
        writeln!(log, "{ticks},{duration},{},{}", source.0, source.1)?;
        let expected = inputs
            .partition_point(|ms| {
                u128::from(*ms).saturating_mul(1_000_000) <= decoded.at.as_nanos()
            })
            .saturating_sub(1);
        assert_eq!(
            source.0, expected,
            "Presentation at {ticks} used a future or stale captured image"
        );
    }
    assert!(bytes > 0);
    Ok(())
}

#[test]
#[ignore = "Opt in: native encoder/decoder with synthetic pixels; no capture or input"]
fn pending_images_and_quiet_output_keep_capture_and_presentation_distinct() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    for stop in [true, false] {
        let path = directory
            .path()
            .join(if stop { "stopped.mp4" } else { "coalesced.mp4" });
        let mut encoder = Encoder::open(&path, canvas::Canvas::new([320, 240])?, FILE_LIMIT)?;
        assert!(
            encoder
                .write(&frame(320, 240, 40), Duration::ZERO, &|| false)?
                .is_continue()
        );
        for (ms, shade) in [(31, 80), (32, 120)] {
            assert!(
                encoder
                    .write(&frame(320, 240, shade), Duration::from_millis(ms), &|| {
                        false
                    })?
                    .is_continue()
            );
        }
        assert!(
            encoder
                .write(&frame(320, 240, 200), Duration::from_millis(30), &|| false)
                .is_err()
        );
        assert_eq!(encoder.coverage().unwrap().last, Duration::ZERO);
        assert_eq!(encoder.coverage().unwrap().frames, 1);
        if !stop {
            assert!(
                encoder
                    .advance(End::Through(Duration::from_millis(34)), &|| false)?
                    .is_continue()
            );
            assert_eq!(encoder.coverage().unwrap().last, Duration::from_millis(32));
            assert_eq!(encoder.coverage().unwrap().frames, 2);
        }
        encoder.finish()?;
        let decoded = read::frame(
            std::fs::File::open(&path)?,
            [320, 240],
            Duration::from_millis(if stop { 32 } else { 34 }),
            &|| false,
        )?
        .unwrap();
        assert_eq!(decoded.at.as_nanos(), if stop { 0 } else { 33_333_300 });
        let expected: u8 = if stop { 40 } else { 120 };
        // H.264 is lossy; the synthetic image identities are at least 40 gray levels apart.
        assert!(
            decoded.bgra[0].abs_diff(expected) <= 8,
            "An unpresented or superseded image reached the video"
        );
    }

    let path = directory.path().join("quiet.mp4");
    let mut encoder = Encoder::open(&path, canvas::Canvas::new([320, 240])?, FILE_LIMIT)?;
    assert!(
        encoder
            .write(&frame(320, 240, 80), Duration::ZERO, &|| false)?
            .is_continue()
    );
    for second in 1..=3 {
        assert!(
            encoder
                .advance(End::Through(Duration::from_secs(second)), &|| false)?
                .is_continue()
        );
    }
    let coverage = encoder.coverage().unwrap();
    assert_eq!(coverage.last, Duration::ZERO);
    assert_eq!(coverage.frames, 91);
    let interrupted = directory.path().join("before-finalize.mp4");
    let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
    loop {
        // WriteSample queues work; a complete fragment must arrive without Finalize or more input.
        std::fs::copy(&path, &interrupted)?;
        if encoder::sample_times(&interrupted).is_ok_and(|samples| samples.len() >= 30) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Quiet capture did not save a fragment before finalization"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let decoded = read::frame(
        std::fs::File::open(&interrupted)?,
        [320, 240],
        Duration::from_millis(500),
        &|| false,
    )?
    .unwrap();
    assert!(decoded.bgra[0].abs_diff(80) <= 8);
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
            .write(&frame(320, 240, 50), Duration::ZERO, &|| false)?
            .is_continue()
    );
    #[expect(
        clippy::disallowed_types,
        reason = "The immutable cancellation callback counts observations on this test's native thread"
    )]
    let checks = std::cell::Cell::new(0_u32);
    let result = encoder.advance(End::Through(Duration::from_secs(1)), &|| {
        let count = checks.get();
        checks.set(count.saturating_add(1));
        count == 3
    })?;
    assert!(result.is_break());
    let coverage = encoder.coverage().unwrap();
    assert_eq!(coverage.frames, 4);
    assert_eq!(coverage.last, Duration::ZERO);
    assert_eq!(coverage.at, Duration::from_millis(100));
    encoder.finish()?;
    assert_eq!(encoder::sample_times(&path)?.len(), 4);
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
    );
    let (_stop, stopping) = async_channel::bounded(1);
    let native = crate::OwnedThread::spawn("synthetic movie stall", move || {
        let image = frame(320, 240, 80);
        let at = image.at;
        let mut movie = Movie::new(Some(target));
        assert!(movie.frame(&image, &stopping));
        assert!(!movie.poll(at.checked_add(Duration::from_secs(3)).unwrap(), &stopping));
        assert!(!movie.active());
    })?;
    let Some(crate::MovieEvent::Started { at, accepted, .. }) = control.next().await else {
        anyhow::bail!("Synthetic movie did not request its origin");
    };
    accepted.send(true).await?;
    native.join().await;
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
    assert_eq!(encoder::sample_times(&path)?.len(), 61);
    Ok(())
}
