use super::*;

#[test]
#[ignore = "Opt in: records only the owned, blacked-out scene into a temporary silent movie"]
fn a_movie_survives_still_backpressure_and_closes_at_the_session_deadline() -> anyhow::Result<()> {
    per_monitor_aware()?;
    let display = display_under_pointer()?;
    let mut scene = Scene::open(display.native)?;
    thread::sleep(COMPOSED);
    let area = scene.area()?;
    scene.check_owned(area)?;
    let part = display
        .native
        .bounds()
        .part_covered_by(Bounds::from(area))
        .context("The owned scene is off screen")?;
    let target = Target {
        display,
        region: Region::Area(part),
    };
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("owned.mp4");
    let started = Instant::now();
    let deadline = started.checked_add(Duration::from_secs(4)).unwrap();
    let (control, movie) = crate::MovieControl::new(path.clone(), deadline);
    let (outgoing, frames) = async_channel::bounded(1);
    let capture = ScreenCapture::start(
        target,
        Duration::from_millis(200),
        u32::MAX,
        outgoing,
        Some(movie),
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let result = runtime.block_on(async {
        let Some(crate::MovieEvent::Started { accepted, at, size }) =
            tokio::time::timeout(FIRST_FRAME, control.next()).await?
        else {
            bail!("Owned movie did not request acceptance");
        };
        assert!(at >= started && at < deadline);
        assert_eq!(
            size,
            [
                (area.right - area.left).cast_unsigned(),
                (area.bottom - area.top).cast_unsigned()
            ]
        );
        scene.check_owned(area)?;
        // This synthetic receipt stands in for the app keeper's separately tested durable header.
        let receipt = std::fs::File::create_new(directory.path().join("origin-receipt"))?;
        receipt.sync_all()?;
        accepted.send(true).await?;
        tokio::time::timeout(FIRST_FRAME, async {
            while !frames.is_full() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let changed = Instant::now();
        scene.open_pattern()?;
        let Some(crate::MovieEvent::Finished {
            last,
            through,
            frames: submitted,
            finalized,
            ..
        }) = tokio::time::timeout(Duration::from_secs(5), control.next()).await?
        else {
            bail!("Owned movie did not finish");
        };
        assert_eq!(through, deadline);
        assert!(
            submitted > 1 && last >= changed,
            "The full still queue stopped later movie frames"
        );
        assert!(finalized);
        assert!(
            frames.is_full(),
            "The still lane never exercised backpressure"
        );
        assert!(control.next().await.is_none());
        anyhow::Ok(())
    });
    drop(capture);
    result?;
    scene.check_owned(area)?;
    let frame = frame_of(frames.try_recv()?)?;
    assert_eq!(
        Crop::of(frame, display.native, area)?.mismatches(Shown::Neither),
        0
    );
    std::fs::rename(&path, directory.path().join("closed.mp4"))?;
    println!(
        "Owned capture and movie closed after {:?}",
        started.elapsed()
    );
    Ok(())
}
