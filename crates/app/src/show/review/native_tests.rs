//! Opt-in owned-window review acceptance. Actions are invoked on the view, never through OS input.

use std::{
    fs,
    time::{Duration, Instant},
};

use anyhow::ensure;
use gpui::{Application, AsyncApp, Render, Timer, div, prelude::*};

use super::*;

#[test]
#[ignore = "Opt-in owned windows; Linux requires a private X11 display. No microphone, input, or clipboard"]
fn native_show_review_navigates_damage_and_releases_private_images() -> anyhow::Result<()> {
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
                completed.send(exercise(cx).await).unwrap();
                cx.update_if_running(|cx| cx.quit());
            })
            .detach();
        });
    outcome
        .try_recv()
        .context("Review acceptance did not complete")?
}

struct TestEdits(super::super::edits::Edits);
impl gpui::Global for TestEdits {}

struct Keeper;
impl Render for Keeper {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        div()
    }
}

#[expect(
    clippy::future_not_send,
    reason = "Native review acceptance uses GPUI's UI-thread context"
)]
async fn exercise(cx: &mut AsyncApp) -> anyhow::Result<()> {
    cx.update(|cx| cx.set_global(TestEdits(super::super::edits::Edits::default())))?;
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
                size(px(1_100.0), px(640.0)),
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Speakeasy Shows acceptance".into()),
                ..Default::default()
            }),
            focus: false,
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                Review::new(
                    Source::Demo,
                    Theme::default(),
                    snapshots,
                    Actions {
                        edit: |edit, fixture, cx| {
                            cx.global_mut::<TestEdits>().0.submit(edit, fixture)
                        },
                        can_copy: |_| false,
                        recover: |_, _| {
                            let (done, receipt) = async_channel::bounded(1);
                            done.try_send(Err(
                                "Synthetic recovery failed; the checkpoint remains.".into(),
                            ))
                            .map_err(|_closed| "Synthetic receipt closed")?;
                            Ok(receipt)
                        },
                    },
                    window,
                    cx,
                )
            })
        },
    )?;
    window.update(cx, |view, _, cx| view.refresh(cx))?;
    wait_for(
        window,
        |view| matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    #[cfg(target_os = "linux")]
    pixels(window, cx).await?;
    // Focus starts on the root: Refresh, three Show rows, then Next (Previous is disabled).
    for _ in 0..5 {
        key(window, "tab", cx)?;
    }
    // GPUI activates clicks on key-up. App handlers must not also activate on key-down.
    for stroke in ["enter", "space"] {
        key(window, stroke, cx)?;
        let frame = window.read_with(cx, |view, _| {
            view.library.selected.as_ref().map(|key| key.frame)
        })?;
        ensure!(
            frame == Some(0),
            "A key-down duplicated GPUI's key-up activation"
        );
    }
    let root = window
        .read_with(cx, |view, _| view.directory.clone())?
        .context("The loaded preview has a directory")?;
    let first = window
        .read_with(cx, |view, _| view.library.selected.clone())?
        .context("The loaded preview selected a Show")?
        .directory;
    let mut journal = speakeasy_show::Journal::new();
    for at in [100, 700, 1_500, 30_000] {
        journal
            .admit()
            .context("Synthetic frame was not admitted")?
            .record(
                speakeasy_show::Moment::from_millis(at),
                speakeasy_show::Image::from_bgra(3, 2, [90, 40, 10, 255].repeat(6))?,
            );
    }
    let mut encoded = Vec::new();
    journal.write_source::<speakeasy_show::SourceError>(
        speakeasy_show::Moment::from_millis(31_000),
        |bytes| {
            encoded.extend_from_slice(bytes);
            Ok(())
        },
    )?;
    fs::write(first.join("source.frames"), encoded)?;
    window.update(cx, |view, window, cx| {
        view.act(Action::Mode(Mode::Source), window, cx);
    })?;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.read_with(cx, |view, _| {
        let detail = view.detail.as_ref();
        assert_eq!(detail.map(|detail| detail.frames.len()), Some(4));
        assert_eq!(
            detail
                .and_then(|detail| detail.frames.first())
                .map(|frame| frame.at.as_millis()),
            Some(100)
        );
        assert!(detail.is_some_and(|detail| detail.coverage.contains("28500 ms")));
    })?;
    let request = window.update(cx, |view, window, cx| {
        view.act(Action::Pane(Pane::Narration), window, cx);
        view.act(Action::Clause(0), window, cx);
        let request = view.request.context("Narration did not select a frame")?;
        let detail = view
            .detail
            .as_ref()
            .context("Narration detail was dropped")?;
        let index = view
            .library
            .selected
            .as_ref()
            .context("Narration selection was dropped")?
            .frame;
        assert_eq!(request.origin, Origin::Clause(0));
        assert_eq!(
            Some(index),
            detail.locate(request.at).map(|(index, _)| index)
        );
        assert!(
            request
                .caption(
                    detail
                        .frames
                        .get(index)
                        .context("Narration selected a missing frame")?,
                    view.mode
                )
                .contains("retained sample")
        );
        view.copy(CopyKind::Moment, cx);
        assert!(
            view.copying.is_none(),
            "Preview copied to the real clipboard"
        );
        view.act(Action::Mode(Mode::Keyframes), window, cx);
        anyhow::Ok(request)
    })??;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.update(cx, |view, window, cx| {
        assert_eq!(view.request, Some(request));
        assert_eq!(
            view.library.selected.as_ref().map(|key| key.frame),
            view.detail
                .as_ref()
                .and_then(|detail| detail.locate(request.at))
                .map(|(index, _)| index)
        );
        view.act(Action::Mode(Mode::Source), window, cx);
    })?;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.update(cx, |view, window, cx| {
        assert_eq!(view.request, Some(request));
        view.act(Action::Next, window, cx);
        assert!(
            view.request.is_none(),
            "Frame navigation kept a stale narration request"
        );
        view.act(Action::Clause(0), window, cx);
        view.act(Action::Refresh, window, cx);
        assert!(
            view.request.is_none(),
            "Refresh kept a stale narration request"
        );
    })?;
    wait_for(
        window,
        |view| {
            view.listing.is_none()
                && view.opening.is_none()
                && matches!(view.image, ImageState::Ready(_))
        },
        cx,
    )
    .await?;
    window.update(cx, |view, window, cx| {
        let detail = view.detail.as_ref().context("The preview has loaded")?;
        let (number, at) = *detail
            .marks
            .first()
            .context("The preview contains a mark")?;
        let expected = detail.locate(at).context("Mark has no frame")?.0;
        view.act(Action::Mark(number), window, cx);
        assert_eq!(
            view.library.selected.as_ref().map(|key| key.frame),
            Some(expected)
        );
        assert_eq!(
            view.request.map(|request| request.origin),
            Some(Origin::Mark(number))
        );
        view.act(Action::Frame(2), window, cx);
        view.act(Action::Jump(true), window, cx);
        assert_eq!(view.library.selected.as_ref().map(|key| key.frame), Some(3));
        view.act(Action::Jump(false), window, cx);
        assert_eq!(view.library.selected.as_ref().map(|key| key.frame), Some(2));
        view.act(Action::Mode(Mode::Keyframes), window, cx);
        anyhow::Ok(())
    })??;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.read_with(cx, |view, _| {
        assert_eq!(
            view.detail.as_ref().map(|detail| detail.frames.len()),
            Some(4)
        );
        assert_eq!(
            view.library.selected.as_ref().map(|key| key.frame),
            view.detail
                .as_ref()
                .and_then(|detail| detail.locate(speakeasy_show::Moment::from_millis(1_500)))
                .map(|(index, _)| index)
        );
    })?;
    window.update(cx, |view, window, cx| {
        view.act(Action::Keep, window, cx);
        view.act(Action::Show(1), window, cx);
    })?;
    wait_for(
        window,
        |view| view.keeping.is_none() && view.opening.is_none(),
        cx,
    )
    .await?;
    window.read_with(cx, |view, _| {
        assert!(
            view.notice.is_none(),
            "A retention result changed another Show's notice"
        );
        assert_eq!(
            view.library
                .rows
                .iter()
                .find(|row| row.directory == first)
                .and_then(|row| row.pinned),
            Some(false)
        );
    })?;
    assert!(!first.join(".pinned").exists());
    window.update(cx, |view, window, cx| view.act(Action::Show(0), window, cx))?;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.update(cx, |view, window, cx| {
        view.act(Action::Next, window, cx);
        view.act(Action::Frame(3), window, cx);
    })?;
    wait_for(
        window,
        |view| {
            view.library
                .selected
                .as_ref()
                .is_some_and(|key| key.frame == 3)
                && matches!(view.image, ImageState::Ready(_))
        },
        cx,
    )
    .await?;
    window.update(cx, |view, window, cx| view.act(Action::Show(2), window, cx))?;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.update(cx, |view, window, cx| {
        view.act(Action::Frame(2), window, cx);
    })?;
    wait_for(
        window,
        |view| matches!(view.image, ImageState::Failed(_)),
        cx,
    )
    .await?;
    let selected = window
        .read_with(cx, |view, _| view.library.selected.clone())?
        .context("The missing frame must remain selected")?;
    ensure!(selected.frame == 2, "A missing frame selected a substitute");
    let directory = selected.directory;
    window.update(cx, |view, window, cx| {
        view.check_selected(cx);
        view.act(Action::Keep, window, cx);
        view.act(Action::Refresh, window, cx);
        view.check_selected(cx);
        assert!(
            view.checking.is_none() && view.listing.is_none(),
            "A read can overwrite an in-flight retention change"
        );
    })?;
    wait_for(window, |view| view.keeping.is_none(), cx).await?;
    window.read_with(cx, |view, _| {
        assert_eq!(
            view.library
                .rows
                .iter()
                .find(|row| row.directory == directory)
                .and_then(|row| row.pinned),
            Some(true),
            "An older read overwrote the retention result"
        );
    })?;
    ensure!(
        directory.join(".pinned").is_file(),
        "The Keep action did not keep the selected Show"
    );
    fs::remove_file(directory.join(".pinned"))?;
    fs::write(directory.join("summary.md"), "Updated synthetic summary")?;
    window.update(cx, |view, _, cx| view.check_selected(cx))?;
    wait_for(
        window,
        |view| {
            view.detail
                .as_ref()
                .is_some_and(|detail| detail.summary.as_deref() == Ok("Updated synthetic summary"))
        },
        cx,
    )
    .await?;
    window.read_with(cx, |view, _| {
        assert_eq!(
            view.library
                .rows
                .iter()
                .find(|row| row.directory == directory)
                .and_then(|row| row.pinned),
            Some(false),
            "Activation did not reread retention changed outside the viewer"
        );
    })?;
    fs::remove_dir_all(directory)?;
    window.update(cx, |view, _, cx| view.refresh(cx))?;
    wait_for(window, |view| view.listing.is_none(), cx).await?;
    window.read_with(cx, |view, _| {
        assert!(view.library.gone && view.library.selected.is_none());
        assert!(view.detail.is_none() && matches!(view.image, ImageState::Empty));
    })?;
    window.update(cx, |view, window, cx| view.act(Action::Show(0), window, cx))?;
    wait_for(
        window,
        |view| matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    let (checkpoint, writer) = super::super::Keeper::fixture(
        &root,
        SystemTime::now(),
        1_000,
        speakeasy_show::Size {
            width: 80.0,
            height: 60.0,
        },
    )?;
    ensure!(writer.close().await, "Synthetic checkpoint did not close");
    let checkpoint = checkpoint.close().await?;
    let interrupted = checkpoint.interrupted()?;
    window.update(cx, |view, _, cx| view.refresh(cx))?;
    wait_for(window, |view| view.listing.is_none(), cx).await?;
    window.update(cx, |view, window, cx| view.act(Action::Show(0), window, cx))?;
    window.read_with(cx, |view, _| {
        assert!(view.interrupted().is_some());
        assert!(view.detail.is_none());
        assert!(matches!(view.image, ImageState::Empty));
    })?;
    window.update(cx, |view, window, cx| view.act(Action::Recover, window, cx))?;
    wait_for(window, |view| view.recovering.is_none(), cx).await?;
    ensure!(
        interrupted.directory().exists(),
        "Failed recovery discarded its checkpoint"
    );
    window.update(cx, |view, window, cx| view.act(Action::Discard, window, cx))?;
    ensure!(
        interrupted.directory().exists(),
        "Discard ran before confirmation"
    );
    window.update(cx, |view, window, cx| view.act(Action::Discard, window, cx))?;
    ensure!(
        interrupted.directory().exists(),
        "A double activation bypassed confirmation"
    );
    window.update(cx, |view, window, cx| {
        view.act(Action::ConfirmDiscard, window, cx);
    })?;
    wait_for(
        window,
        |view| view.discarding.is_none() && view.listing.is_none(),
        cx,
    )
    .await?;
    ensure!(
        !interrupted.directory().exists(),
        "Confirmed discard left its checkpoint"
    );
    window.update(cx, |view, window, cx| view.act(Action::Show(0), window, cx))?;
    wait_for(
        window,
        |view| matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    let pixels = window
        .read_with(cx, |view, _| match &view.image {
            ImageState::Ready(image) => Some(Arc::downgrade(image)),
            _ => None,
        })?
        .context("The wait established a decoded frame")?;
    let (fixture, kept_directory) = window.update(cx, |view, window, cx| {
        let fixture = view.fixture.take();
        let directory = view
            .library
            .selected
            .as_ref()
            .map(|key| key.directory.clone());
        view.keep(cx);
        window.remove_window();
        (fixture, directory)
    })?;
    let kept_directory = kept_directory.context("The closing window selected a Show")?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .context("A short deadline fits")?;
    while !kept_directory.join(".pinned").exists() && Instant::now() < deadline {
        Timer::after(Duration::from_millis(10)).await;
    }
    ensure!(
        kept_directory.join(".pinned").is_file(),
        "Closing review cancelled an accepted Keep action"
    );
    drop(fixture);
    Timer::after(Duration::from_millis(100)).await;
    ensure!(
        pixels.upgrade().is_none(),
        "Closing review retained its image in memory"
    );
    ensure!(
        !root.exists(),
        "Closing review retained its synthetic library"
    );
    ensure!(
        keeper.read_with(cx, |_, _| ()).is_ok(),
        "Closing review also closed another app window"
    );
    Ok(())
}

fn key(window: WindowHandle<Review>, stroke: &str, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let keystroke = gpui::Keystroke::parse(stroke)?;
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    })
}

async fn wait_for(
    window: WindowHandle<Review>,
    ready: impl Fn(&Review) -> bool,
    cx: &AsyncApp,
) -> anyhow::Result<()> {
    let start = Instant::now();
    while !window.read_with(cx, |view, _| ready(view))? {
        ensure!(
            start.elapsed() < Duration::from_secs(30),
            "Review did not reach the expected state"
        );
        Timer::after(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::future_not_send,
    reason = "The owned X11 window is resolved through GPUI's UI context"
)]
async fn pixels(window: WindowHandle<Review>, cx: &mut AsyncApp) -> anyhow::Result<()> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use x11rb::{
        protocol::xproto::{ConnectionExt as _, ImageFormat},
        xcb_ffi::XCBConnection,
    };
    let id = window.update(cx, |_, window, _| -> anyhow::Result<u32> {
        match HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow::anyhow!("Owned window handle: {error}"))?
            .as_raw()
        {
            RawWindowHandle::Xcb(handle) => Ok(handle.window.get()),
            RawWindowHandle::Xlib(handle) => Ok(u32::try_from(handle.window)?),
            _ => anyhow::bail!("Review acceptance needs X11"),
        }
    })??;
    let (connection, _) = XCBConnection::connect(None)?;
    let start = Instant::now();
    loop {
        let bounds = connection.get_geometry(id)?.reply()?;
        let image = connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                id,
                0,
                0,
                bounds.width,
                bounds.height,
                u32::MAX,
            )?
            .reply()?;
        // The public fixture's light Settings panel covers far more area than any text in the
        // dark review window. Read only this owned window, never the root or another app's pixels.
        let light = image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel.iter().take(3).all(|channel| *channel > 200))
            .count();
        if light > 10_000 {
            return Ok(());
        }
        ensure!(
            start.elapsed() < Duration::from_secs(10),
            "Review did not paint the synthetic frame"
        );
        Timer::after(Duration::from_millis(20)).await;
    }
}
