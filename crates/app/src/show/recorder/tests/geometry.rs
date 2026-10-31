use super::*;

#[test]
fn pointer_geometry_does_not_wait_for_a_still_or_rewind_when_one_arrives() -> anyhow::Result<()> {
    let started = Instant::now();
    let at = |ms| started.checked_add(Duration::from_millis(ms)).unwrap();
    let (canvas, _) = watch::channel(Canvas::default());
    let mut take = Take::new(started, [1_000.0, 500.0], Part::WHOLE, &canvas);
    let part = Part::between([0.25, 0.25], [0.75, 0.75]);
    for (ms, pressed) in [(300, true), (320, false)] {
        take.pointer(PointerSample::Observed {
            at: at(ms),
            position: [0.375, 0.5],
            part,
            drawing: false,
            pressed,
        })?;
    }
    take.frame(Frame {
        at: at(200),
        width: 4,
        height: 4,
        bgra: vec![0x80; 64],
        part: Part::WHOLE,
    })?;
    let (recording, _) = take.state.finish();
    assert!(matches!(
        recording.actions.as_slice(),
        [Action::Click { point, .. }] if *point == Point::new(0.25, 0.5)
    ));
    assert_eq!(
        recording.surface,
        Size {
            width: 500.0,
            height: 250.0
        }
    );
    Ok(())
}

#[test]
fn later_frames_do_not_change_queued_pointer_distance_thresholds() -> anyhow::Result<()> {
    let started = Instant::now();
    let at = |ms| started.checked_add(Duration::from_millis(ms)).unwrap();
    let (canvas, _) = watch::channel(Canvas::default());
    let mut take = Take::new(started, [1_000.0, 500.0], Part::WHOLE, &canvas);
    take.frame(Frame {
        at: at(500),
        width: 4,
        height: 4,
        bgra: vec![0x80; 64],
        part: Part::WHOLE,
    })?;
    let part = Part::between([0.25, 0.25], [0.75, 0.75]);
    for (ms, x, pressed) in [(300, 0.375, true), (310, 0.385, true), (320, 0.385, false)] {
        take.pointer(PointerSample::Observed {
            at: at(ms),
            position: [x, 0.5],
            part,
            drawing: false,
            pressed,
        })?;
    }
    let (recording, _) = take.state.finish();
    assert!(matches!(
        recording.actions.as_slice(),
        [Action::Click { point, .. }] if *point == Point::new(0.25, 0.5)
    ));
    assert_eq!(
        recording.surface,
        Size {
            width: 1_000.0,
            height: 500.0
        }
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn finishing_keeps_known_pointer_gaps_but_excludes_teardown_observations()
-> anyhow::Result<()> {
    for before_end in [true, false] {
        let (recorded, _) = record_with(
            |take| {
                take.frame(Frame {
                    at: take.started,
                    width: 4,
                    height: 4,
                    bgra: vec![0x80; 64],
                    part: Part::WHOLE,
                })
                .unwrap();
                take.pointer(PointerSample::Observed {
                    at: take.started,
                    position: [0.5, 0.5],
                    part: Part::WHOLE,
                    drawing: false,
                    pressed: true,
                })
                .unwrap();
            },
            async |feed| {
                wait(100).await;
                if before_end {
                    feed.samples
                        .send(PointerSample::Gap(feed.at(90)))
                        .await
                        .unwrap();
                }
                feed.finish().await;
                if !before_end {
                    wait(80).await;
                    feed.samples
                        .send(PointerSample::Gap(feed.at(180)))
                        .await
                        .unwrap();
                }
            },
        )
        .await;
        let recording = recorded?.context("The synthetic take completed")?.recording;
        assert_eq!(recording.duration, Moment::from_millis(100));
        assert_eq!(recording.actions.len(), usize::from(!before_end));
    }
    Ok(())
}
