use std::{
    fs,
    time::{Duration, Instant},
};

use anyhow::ensure;
use gpui::{AsyncApp, Pixels, Render, RenderImage, Size, div, prelude::*};

use super::*;

#[cfg(target_os = "windows")]
mod video;

#[test]
#[ignore = "Opt-in owned windows; Linux requires a private X11 display. No microphone, input, or clipboard"]
fn native_show_review_navigates_damage_and_releases_private_images() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _entered = runtime.enter();
    let (completed, outcome) = std::sync::mpsc::channel();
    crate::application::new()
        .unwrap()
        .with_assets(crate::icons::Icons)
        .run(move |cx| {
            crate::load_brand_font(cx);
            cx.spawn(async move |cx| {
                completed.send(exercise(cx).await).unwrap();
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    outcome
        .try_recv()
        .context("Review acceptance did not complete")?
}

struct TestReads(super::super::preview::Reads);
impl gpui::Global for TestReads {}

struct TestEdits(super::super::edits::Edits);
impl gpui::Global for TestEdits {}

struct CopyProbe {
    image: std::sync::Weak<RenderImage>,
    summary: String,
    narration: String,
    received: bool,
    agent: Option<std::path::PathBuf>,
}
impl gpui::Global for CopyProbe {}

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
    cx.update(|cx| cx.set_global(TestEdits(super::super::edits::Edits::default())));
    cx.update(|cx| cx.set_global(TestReads(super::super::preview::Reads::default())));
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
                        preview: |request, cx| cx.global_mut::<TestReads>().0.submit(request),
                        can_copy: |_| false,
                        copy: |_, _| anyhow::bail!("Synthetic review does not use the clipboard"),
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
    export_selected(window, cx).await?;
    copy_selected(window, cx).await?;

    window.update(cx, |view, _, cx| {
        view.act(Action::Mode(Mode::Keyframes), cx);
        view.act(Action::Frame(0), cx);
    })?;
    wait_for(
        window,
        |view| matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))?;
    let next = window.update(cx, |view, window, cx| {
        let detail = view
            .detail
            .as_ref()
            .context("The key fixture has no frames")?;
        ensure!(
            detail.frames.len() >= 3,
            "Next needs at least three fixture frames"
        );
        let tabs = view
            .library
            .rows
            .len()
            .checked_add(5)
            .and_then(|tabs| tabs.checked_add(usize::from(detail.source_available)))
            .and_then(|tabs| {
                tabs.checked_add(usize::from(
                    cfg!(any(target_os = "windows", target_os = "macos")) && detail.movie.is_some(),
                ))
            })
            .context("The fixture's tab stops fit usize")?;
        view.focus.focus(window, cx);
        for _ in 0..tabs {
            window.focus_next(cx);
        }
        window
            .focused(cx)
            .context("Next did not receive keyboard focus")
    })??;
    for (stroke, before, after) in [("enter", 0, 1), ("space", 1, 2)] {
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))?;
        window.update(cx, |_, window, _| {
            ensure!(
                next.is_focused(window),
                "Next lost focus between activations"
            );
            anyhow::Ok(())
        })??;
        key(window, stroke, cx)?;
        let frame = window.read_with(cx, |view, _| {
            view.library.selected.as_ref().map(|key| key.frame)
        })?;
        ensure!(
            frame == Some(before),
            "Key-down activated Next before key-up"
        );
        release(window, stroke, cx)?;
        let frame = window.read_with(cx, |view, _| {
            view.library.selected.as_ref().map(|key| key.frame)
        })?;
        ensure!(
            frame == Some(after),
            "Next did not activate exactly once on key-up"
        );
    }
    more_actions(window, cx)?;
    selection_in_view(window, cx).await?;
    window.update(cx, |view, _, cx| {
        view.act(Action::Frame(0), cx);
    })?;
    wait_for(
        window,
        |view| matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
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
    window.update(cx, |view, _, cx| {
        view.act(Action::Mode(Mode::Source), cx);
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
    let request = window.update(cx, |view, _, cx| {
        view.act(Action::Pane(Pane::Narration), cx);
        view.act(Action::Clause(0), cx);
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
                    view.mode,
                    detail
                        .frames
                        .get(index)
                        .context("Missing synthetic frame")?
                        .presentation()
                )
                .contains("retained sample")
        );
        view.copy(CopyKind::Evidence, cx);
        assert!(
            view.copying.is_none(),
            "Preview copied to the real clipboard"
        );
        view.act(Action::Mode(Mode::Keyframes), cx);
        anyhow::Ok(request)
    })??;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.update(cx, |view, _, cx| {
        assert_eq!(view.request, Some(request));
        assert_eq!(
            view.library.selected.as_ref().map(|key| key.frame),
            view.detail
                .as_ref()
                .and_then(|detail| detail.locate(request.at))
                .map(|(index, _)| index)
        );
        view.act(Action::Mode(Mode::Source), cx);
    })?;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.update(cx, |view, _, cx| {
        assert_eq!(view.request, Some(request));
        view.act(Action::Next, cx);
        assert!(
            view.request.is_none(),
            "Frame navigation kept a stale narration request"
        );
        view.act(Action::Clause(0), cx);
        view.act(Action::Refresh, cx);
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
    window.update(cx, |view, _, cx| {
        let detail = view.detail.as_ref().context("The preview has loaded")?;
        let (number, at) = *detail
            .marks
            .first()
            .context("The preview contains a mark")?;
        let expected = detail.locate(at).context("Mark has no frame")?.0;
        view.act(Action::Mark(number), cx);
        assert_eq!(
            view.library.selected.as_ref().map(|key| key.frame),
            Some(expected)
        );
        assert_eq!(
            view.request.map(|request| request.origin),
            Some(Origin::Mark(number))
        );
        view.act(Action::Frame(2), cx);
        view.act(Action::Jump(Direction::Forward), cx);
        assert_eq!(view.library.selected.as_ref().map(|key| key.frame), Some(3));
        view.act(Action::Jump(Direction::Back), cx);
        assert_eq!(view.library.selected.as_ref().map(|key| key.frame), Some(2));
        view.act(Action::Mode(Mode::Keyframes), cx);
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
            Some(5)
        );
        assert_eq!(
            view.library.selected.as_ref().map(|key| key.frame),
            view.detail
                .as_ref()
                .and_then(|detail| detail.locate(speakeasy_show::Moment::from_millis(1_500)))
                .map(|(index, _)| index)
        );
    })?;
    window.update(cx, |view, _, cx| {
        view.act(Action::Keep, cx);
        view.act(Action::Show(1), cx);
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
    window.update(cx, |view, _, cx| view.act(Action::Show(0), cx))?;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.update(cx, |view, _, cx| {
        view.act(Action::Next, cx);
        view.act(Action::Frame(3), cx);
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
    window.update(cx, |view, _, cx| view.act(Action::Show(2), cx))?;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    window.update(cx, |view, _, cx| {
        view.act(Action::Frame(2), cx);
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
    window.update(cx, |view, _, cx| {
        view.check_selected(cx);
        view.act(Action::Keep, cx);
        view.act(Action::Refresh, cx);
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
    window.update(cx, |view, _, cx| view.act(Action::Show(0), cx))?;
    wait_for(
        window,
        |view| matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    let vanished = window
        .read_with(cx, |view, _| {
            view.library
                .selected
                .as_ref()
                .map(|key| key.directory.clone())
        })?
        .context("The loaded preview selected a Show")?;
    fs::remove_dir_all(&vanished)?;
    window.update(cx, |view, _, cx| view.act(Action::Keep, cx))?;
    wait_for(window, |view| view.keeping.is_none(), cx).await?;
    window.read_with(cx, |view, _| {
        assert!(view.library.gone && view.library.selected.is_none());
        assert!(view.detail.is_none() && matches!(view.image, ImageState::Empty));
        assert!(
            view.library
                .rows
                .iter()
                .all(|row| row.directory != vanished)
        );
    })?;
    window.update(cx, |view, _, cx| view.act(Action::Show(0), cx))?;
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
    let checkpoint = checkpoint
        .close()
        .await?
        .context("Fixture checkpoint is missing")?;
    let interrupted = checkpoint.interrupted()?;
    window.update(cx, |view, _, cx| view.refresh(cx))?;
    wait_for(window, |view| view.listing.is_none(), cx).await?;
    window.update(cx, |view, _, cx| view.act(Action::Show(0), cx))?;
    window.read_with(cx, |view, _| {
        assert!(view.interrupted().is_some());
        assert!(view.detail.is_none());
        assert!(matches!(view.image, ImageState::Empty));
    })?;
    window.update(cx, |view, _, cx| view.act(Action::Recover, cx))?;
    wait_for(window, |view| view.recovering.is_none(), cx).await?;
    ensure!(
        interrupted.directory().exists(),
        "Failed recovery discarded its checkpoint"
    );
    window.update(cx, |view, _, cx| view.act(Action::Discard, cx))?;
    ensure!(
        interrupted.directory().exists(),
        "Discard ran before confirmation"
    );
    window.update(cx, |view, _, cx| view.act(Action::Discard, cx))?;
    ensure!(
        interrupted.directory().exists(),
        "A double activation bypassed confirmation"
    );
    window.update(cx, |view, _, cx| {
        view.act(Action::ConfirmDiscard, cx);
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
    window.update(cx, |view, _, cx| view.act(Action::Show(0), cx))?;
    wait_for(
        window,
        |view| matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    let pixels = window
        .read_with(cx, |view, _| match &view.image {
            ImageState::Ready(decoded) => Some(Arc::downgrade(&decoded.image)),
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
    let (edits, reads) = cx.update(|cx| {
        (
            cx.global::<TestEdits>().0.drained(),
            cx.global::<TestReads>().0.drained(),
        )
    });
    edits.await;
    reads.await;
    ensure!(
        kept_directory.join(".pinned").is_file(),
        "Closing review cancelled an accepted Keep action"
    );
    drop(fixture);
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

fn key(
    window: WindowHandle<Review>,
    stroke: &'static str,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let keystroke = gpui::Keystroke::parse(stroke)?;
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    })
}

fn release(
    window: WindowHandle<Review>,
    stroke: &'static str,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let keystroke = gpui::Keystroke::parse(stroke)?;
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }),
            cx,
        );
    })
}

fn more_actions(window: WindowHandle<Review>, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let shown = window.update(cx, |view, window, cx| {
        let stops = view
            .library
            .rows
            .len()
            .checked_add(3)
            .context("The fixture's tab stops fit usize")?;
        view.focus.focus(window, cx);
        for _ in 0..stops {
            window.focus_next(cx);
        }
        anyhow::Ok(view.library.selected.clone())
    })??;
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))?;
    key(window, "enter", cx)?;
    release(window, "enter", cx)?;
    let opened = window.read_with(cx, |view, _| view.menu)?;
    ensure!(
        matches!(opened, menu::Menu::Open(Some(_))),
        "Enter on More actions did not open its menu at an available action"
    );
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))?;
    key(window, "down", cx)?;
    let (moved, selected) =
        window.read_with(cx, |view, _| (view.menu, view.library.selected.clone()))?;
    ensure!(
        moved != opened && moved.is_open(),
        "Down did not move within the open menu"
    );
    ensure!(selected == shown, "Down in the open menu changed the Show");
    key(window, "escape", cx)?;
    let closed = window.read_with(cx, |view, _| view.menu)?;
    ensure!(
        closed == menu::Menu::Closed,
        "Escape closed the window instead of the menu"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "The selection acceptance resizes the owned review window on its UI thread"
)]
async fn resize(
    window: WindowHandle<Review>,
    to: Size<Pixels>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    cx.update_window(window.into(), |_, window, _| window.resize(to))?;
    let start = Instant::now();
    while cx.update_window(window.into(), |_, window, _| window.viewport_size())? != to {
        ensure!(
            start.elapsed() < Duration::from_secs(10),
            "The review window did not resize"
        );
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    }
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
}

#[expect(
    clippy::future_not_send,
    reason = "The selection acceptance drives the owned GPUI review view on its UI thread"
)]
async fn select(
    window: WindowHandle<Review>,
    action: Action,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    window.update(cx, |view, _, cx| view.act(action, cx))?;
    wait_for(
        window,
        |view| view.opening.is_none() && matches!(view.image, ImageState::Ready(_)),
        cx,
    )
    .await?;
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
}

#[expect(
    clippy::future_not_send,
    reason = "The selection acceptance resizes the owned review window on its UI thread"
)]
async fn selection_in_view(window: WindowHandle<Review>, cx: &mut AsyncApp) -> anyhow::Result<()> {
    for width in [px(640.0), px(1_100.0)] {
        resize(window, size(width, px(640.0)), cx).await?;
        let last = window.read_with(cx, |view, _| {
            view.detail
                .as_ref()
                .map_or(0, |detail| detail.frames.len().saturating_sub(1))
        })?;
        select(window, Action::Frame(last), cx).await?;
        let (strip, chip, offset) = window.read_with(cx, |view, _| {
            (
                view.frame_scroll.bounds(),
                view.frame_scroll
                    .bounds_for_item(last.min(render::NEARBY_FRAMES)),
                view.frame_scroll.offset().x,
            )
        })?;
        let chip = chip.context("The frame strip has no chip for the selected frame")?;
        ensure!(
            chip.left() + offset >= strip.left() && chip.right() + offset <= strip.right(),
            "The selected frame chip at {width:?} lies outside the visible frame strip"
        );
    }
    let (first, last) = window.read_with(cx, |view, _| {
        (
            view.library.position(),
            view.library.rows.len().saturating_sub(1),
        )
    })?;
    let first = first.context("The review selected a Show")?;
    resize(window, size(px(1_100.0), px(320.0)), cx).await?;
    select(window, Action::Show(last), cx).await?;
    let (library, row, offset) = window.read_with(cx, |view, _| {
        (
            view.library_scroll.bounds(),
            view.library_scroll.bounds_for_item(last),
            view.library_scroll.offset().y,
        )
    })?;
    let row = row.context("The library has no row for the selected Show")?;
    ensure!(
        row.top() + offset >= library.top() && row.bottom() + offset <= library.bottom(),
        "The selected Show row lies outside the visible library"
    );
    resize(window, size(px(1_100.0), px(640.0)), cx).await?;
    select(window, Action::Show(first), cx).await
}

#[expect(
    clippy::future_not_send,
    reason = "The export acceptance drives the owned GPUI review view on its UI thread"
)]
async fn export_selected(window: WindowHandle<Review>, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let destination = root.path().join("show.zip");
    let directory = window
        .read_with(cx, |view, _| {
            view.library
                .selected
                .as_ref()
                .map(|key| key.directory.clone())
        })?
        .context("The export fixture has a selected Show")?;
    window.update(cx, |view, _, cx| {
        view.export_to(
            directory.clone(),
            destination.clone(),
            ExportKind::Archive,
            cx,
        );
    })?;
    wait_for(window, |view| view.exporting.is_none(), cx).await?;
    let success = window.read_with(cx, |view, _| {
        view.notice
            .as_ref()
            .is_some_and(|notice| notice.starts_with("Exported to "))
    })?;
    ensure!(success, "Review did not report a completed export");
    let mut archive = zip::ZipArchive::new(fs::File::open(destination)?)?;
    let mut summary = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_name("summary.md")?, &mut summary)?;
    ensure!(
        summary == fs::read(directory.join("summary.md"))?,
        "Review export changed the saved summary"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "The synthetic handoff drives the owned review view on its UI thread"
)]
async fn copy_selected(window: WindowHandle<Review>, cx: &mut AsyncApp) -> anyhow::Result<()> {
    window.update(cx, |view, _, cx| {
        let ImageState::Ready(decoded) = &view.image else {
            anyhow::bail!("The copy fixture has no displayed image");
        };
        let detail = view
            .detail
            .as_ref()
            .context("The copy fixture has no selected Show")?;
        let summary = detail
            .summary
            .as_ref()
            .map_err(|_error| anyhow::anyhow!("The fixture has no summary"))?
            .clone();
        let narration = detail
            .narration
            .clauses()
            .first()
            .context("The fixture has no recorded clause")?
            .text
            .clone();
        cx.set_global(CopyProbe {
            image: Arc::downgrade(&decoded.image),
            summary,
            narration,
            received: false,
            agent: None,
        });

        view.source = Source::Saved;
        view.actions.can_copy = |_| true;
        view.actions.copy = |request, cx| {
            let super::super::copy::Request::Moment { text, image } = request else {
                anyhow::bail!("Copy moment omitted its image");
            };
            let probe = cx.global_mut::<CopyProbe>();
            let shown = probe
                .image
                .upgrade()
                .context("The displayed image was dropped")?;
            ensure!(
                Arc::ptr_eq(&shown, &image),
                "Copy moment did not retain the displayed pixels"
            );
            ensure!(
                text.contains("Keyframe #") && text.contains("Show folder:"),
                "Copy moment lost its textual context"
            );
            ensure!(
                text.contains(&probe.summary) && text.contains(&probe.narration),
                "Evidence copy omitted the displayed summary or recorded narration"
            );
            probe.received = true;
            Ok(Box::pin(async { Ok(super::super::copy::Outcome::Copied) }))
        };
        view.copy(CopyKind::Evidence, cx);
        view.source = Source::Demo;
        view.actions.can_copy = |_| false;
        view.actions.copy = |_, _| anyhow::bail!("Synthetic review does not use the clipboard");
        anyhow::Ok(())
    })??;
    wait_for(window, |view| view.copying.is_none(), cx).await?;
    ensure!(
        cx.read_global::<CopyProbe, _>(|probe, _| probe.received),
        "The visual copy action did not run"
    );
    let copied = window.read_with(cx, |view, _| {
        view.notice.as_deref() == Some("Copied image, summary, and narration.")
    })?;
    ensure!(
        copied,
        "The review did not report the synthetic copy result"
    );

    let selected = window.update(cx, |view, _, cx| {
        view.source = Source::Saved;
        view.actions.can_copy = |_| true;
        view.actions.copy = |request, cx| {
            let super::super::copy::Request::Agent(directory) = request else {
                anyhow::bail!("Send to agent copied something other than its prompt");
            };
            cx.global_mut::<CopyProbe>().agent = Some(directory);
            Ok(Box::pin(async { Ok(super::super::copy::Outcome::Copied) }))
        };
        view.act(Action::Copy(CopyKind::Agent), cx);
        view.source = Source::Demo;
        view.actions.can_copy = |_| false;
        view.actions.copy = |_, _| anyhow::bail!("Synthetic review does not use the clipboard");
        view.library
            .selected
            .as_ref()
            .map(|key| key.directory.clone())
    })?;
    wait_for(window, |view| view.copying.is_none(), cx).await?;
    let agent = cx.read_global::<CopyProbe, _>(|probe, _| probe.agent.clone());
    ensure!(
        agent.is_some() && agent == selected,
        "Send to agent did not point at the selected Show"
    );
    let announced = window.read_with(cx, |view, _| {
        view.notice
            .as_deref()
            .is_some_and(|notice| notice.starts_with("Copied a prompt for your agent."))
    })?;
    ensure!(announced, "Send to agent did not say what it copied");
    Ok(())
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
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
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
        cx.background_executor()
            .timer(Duration::from_millis(20))
            .await;
    }
}
