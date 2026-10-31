//! Native video review uses recovered synthetic movies and dispatches events only to its owned window.

use gpui::{Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput};

use super::*;
use crate::show::store::movie;

#[test]
#[ignore = "Opt in: owned GPUI window and native decoder; SPEAKEASY_MOVIE_READ_DIRECTORY names synthetic movie prefixes"]
fn native_show_video_review_scrubs_steps_and_releases_its_reader() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _entered = runtime.enter();
    let (completed, outcome) = std::sync::mpsc::channel();
    Application::new()
        .with_assets(crate::icons::Icons)
        .run(move |cx| {
            crate::load_brand_font(cx);
            cx.spawn(async move |cx| {
                completed.send(exercise_video(cx).await).unwrap();
                cx.update_if_running(|cx| cx.quit());
            })
            .detach();
        });
    outcome
        .try_recv()
        .context("Video review acceptance did not complete")?
}

#[expect(
    clippy::future_not_send,
    reason = "Owned native review is exercised on GPUI's UI thread"
)]
async fn exercise_video(cx: &mut AsyncApp) -> anyhow::Result<()> {
    let fixture = Arc::new(tempfile::tempdir()?);
    let directory = fixture.path().join("20261020T143205Z");
    fs::create_dir(&directory)?;
    movie::tests::native_fixture(&directory)?;
    cx.update(|cx| cx.set_global(TestReads(crate::show::preview::Reads::default())))?;
    let keeper = cx.open_window(
        WindowOptions {
            show: false,
            focus: false,
            ..Default::default()
        },
        |_, cx| cx.new(|_| Keeper),
    )?;
    let (_snapshots, snapshots) = watch::channel(Snapshot::default());
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                gpui::point(px(0.0), px(0.0)),
                size(px(1100.0), px(800.0)),
            ))),
            focus: false,
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                let mut review = Review::new(
                    Source::Saved,
                    Theme::default(),
                    snapshots,
                    Actions {
                        preview: |request, cx| cx.global_mut::<TestReads>().0.submit(request),
                        edit: |_, _, _| {
                            anyhow::bail!("Synthetic video review does not edit bundles")
                        },
                        can_copy: |_| false,
                        copy: |_, _| {
                            anyhow::bail!("Synthetic video review does not use the clipboard")
                        },
                        recover: |_, _| Err("Synthetic video review does not recover audio"),
                    },
                    window,
                    cx,
                );
                review.directory = Some(fixture.path().to_owned());
                review.fixture = Some(Arc::clone(&fixture));
                review
            })
        },
    )?;
    drop(fixture);
    window.update(cx, |view, _, cx| view.refresh(cx))?;
    wait_for(window, ready, cx).await?;
    window.update(cx, |view, window, cx| {
        view.act(Action::Mode(Mode::Video), window, cx);
    })?;
    wait_for(
        window,
        |view| ready(view) && view.mode == Mode::Video && view.timeline.bounds.size.width > px(0.0),
        cx,
    )
    .await?;
    window.update(cx, |view, window, cx| {
        view.act(Action::Frame(1), window, cx);
    })?;
    wait_for(window, ready, cx).await?;
    let (reference, image) = window.read_with(cx, |view, _| -> anyhow::Result<_> {
        let ImageState::Ready(decoded) = &view.image else {
            anyhow::bail!("Missing decoded video");
        };
        assert_eq!(decoded.at, Duration::from_nanos(283_333_300));
        let frame = view
            .detail
            .as_ref()
            .and_then(|detail| detail.frames.get(1))
            .context("Missing indexed video frame")?;
        Ok((
            navigation::pointer(&directory, frame, Mode::Video, None, decoded.at),
            Arc::clone(&decoded.image),
        ))
    })??;
    assert!(reference.contains("--video --at 0:00.284"));
    assert!(!reference.contains("PRIVATE"));
    let path = directory.clone();
    let served = cx
        .background_executor()
        .spawn(async move {
            movie::Reading::open(&path, speakeasy_show::Moment::from_millis(1000))?.frame(
                speakeasy_show::Moment::from_millis(284),
                None,
                || false,
            )
        })
        .await?
        .context("Synthetic retrieval was cancelled")?;
    assert_eq!(served.at.as_millis(), 283);
    let mut exported =
        image::load_from_memory_with_format(&served.image.encode_png()?, image::ImageFormat::Png)?
            .into_rgba8();
    for [red, _, blue, _] in exported
        .as_flat_samples_mut()
        .samples
        .as_chunks_mut::<4>()
        .0
    {
        std::mem::swap(red, blue);
    }
    ensure!(
        image.as_bytes(0) == Some(exported.as_raw().as_slice()),
        "Copy moment's retrieval query selected different pixels"
    );
    drop(image);
    window.update(cx, |view, window, cx| {
        for index in 0..120 {
            view.act(Action::Frame(index), window, cx);
        }
    })?;
    wait_for(window, ready, cx).await?;
    window.read_with(cx, |view, _| -> anyhow::Result<()> {
        let ImageState::Ready(decoded) = &view.image else {
            anyhow::bail!("Missing final seek");
        };
        assert_eq!(
            view.library.selected.as_ref().map(|key| key.frame),
            Some(119)
        );
        assert_eq!(decoded.at, Duration::from_nanos(4_216_666_600));
        assert!(!view.video_caption().contains("Seeking"));
        Ok(())
    })??;
    for (action, expected) in [(Action::Mark(1), 750_u64), (Action::Clause(0), 483)] {
        window.update(cx, |view, window, cx| view.act(action, window, cx))?;
        wait_for(window, ready, cx).await?;
        window.read_with(cx, |view, _| -> anyhow::Result<()> {
            let ImageState::Ready(decoded) = &view.image else {
                anyhow::bail!("Missing narration/mark frame");
            };
            assert_eq!(decoded.at.as_millis(), u128::from(expected));
            Ok(())
        })??;
    }
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())?;
    let bounds = window.read_with(cx, |view, _| view.timeline.bounds)?;
    let position = bounds.center();
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
            cx,
        );
    })?;
    wait_for(window, ready, cx).await?;
    window.read_with(cx, |view, _| -> anyhow::Result<()> {
        let ImageState::Ready(decoded) = &view.image else {
            anyhow::bail!("Missing scrubbed frame");
        };
        assert!(
            decoded.at >= Duration::from_millis(2200) && decoded.at <= Duration::from_millis(2250)
        );
        Ok(())
    })??;
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())?;
    let bounds = window.read_with(cx, |view, _| view.timeline.bounds)?;
    let position = bounds.center();
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position: gpui::point(bounds.left() + bounds.size.width * 0.75, position.y),
                pressed_button: Some(MouseButton::Left),
                modifiers: Modifiers::default(),
            }),
            cx,
        );
    })?;
    wait_for(window, ready, cx).await?;
    let dragged = window.read_with(cx, |view, _| view.library.selected.clone())?;
    assert!(
        dragged.as_ref().is_some_and(|key| key.frame >= 89),
        "Dragging did not reach the final quarter: selected {:?}",
        dragged.as_ref().map(|key| key.frame)
    );
    cx.update_window(window.into(), |_, window, cx| {
        for pressed_button in [None, Some(MouseButton::Left)] {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
        }
    })?;
    window.read_with(cx, |view, _| {
        assert_eq!(
            view.library.selected, dragged,
            "Hovering after a lost mouse-up kept seeking"
        );
    })?;
    key(window, "end", cx)?;
    wait_for(window, ready, cx).await?;
    let pixels = window.read_with(cx, |view, _| -> anyhow::Result<_> {
        let ImageState::Ready(decoded) = &view.image else {
            anyhow::bail!("Missing end frame");
        };
        assert_eq!(
            view.library.selected.as_ref().map(|key| key.frame),
            Some(119)
        );
        Ok(Arc::downgrade(&decoded.image))
    })??;
    window.update(cx, |view, window, cx| {
        view.act(Action::Frame(1), window, cx);
        window.remove_window();
    })?;
    let drained = cx.update(|cx| cx.global::<TestReads>().0.drained())?;
    drained.await;
    Timer::after(Duration::from_millis(50)).await;
    assert!(pixels.upgrade().is_none());
    assert!(
        !directory.exists(),
        "Closing review retained its synthetic library after reader cleanup"
    );
    assert!(keeper.read_with(cx, |_, _| ()).is_ok());
    Ok(())
}

fn ready(view: &Review) -> bool {
    view.opening.is_none() && view.loading.is_none() && matches!(view.image, ImageState::Ready(_))
}
