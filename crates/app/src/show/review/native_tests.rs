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
                    |_| false,
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
    let pixels = window
        .read_with(cx, |view, _| match &view.image {
            ImageState::Ready(image) => Some(Arc::downgrade(image)),
            _ => None,
        })?
        .context("The wait established a decoded frame")?;
    window.update(cx, |_, window, _| window.remove_window())?;
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
