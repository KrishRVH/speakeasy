//! Opt-in own-window rendering and frame-source acceptance, without audio, global input or
//! clipboard access.

#[path = "../src/icons.rs"]
mod icons;

use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use gpui::{
    AppContext, Application, AsyncApp, Bounds, PathBuilder, Pixels, Timer, Window, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, canvas, div, point, prelude::*, px, rgb, size, svg,
};
use raw_window_handle::HasWindowHandle;

const TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Lets the native renderer finish submitting a frame or applying a visibility change.
const PRESENT_SETTLE: Duration = Duration::from_millis(20);
const CYCLES: usize = 25;

#[derive(Default)]
struct Scene {
    draws_path: bool,
    renders: usize,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        self.renders = self.renders.saturating_add(1);
        let draws_path = self.draws_path;
        div()
            .size_full()
            .bg(rgb(0x11_33_55))
            .child(
                svg()
                    .path(icons::KEYSTONE)
                    .absolute()
                    .top(px(8.0))
                    .left(px(8.0))
                    .size(px(10.0))
                    .text_color(rgb(0xCC_99_55)),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        if draws_path {
                            paint_diamond(bounds, window);
                        }
                    },
                )
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .size_full(),
            )
    }
}

fn main() -> anyhow::Result<()> {
    ensure!(
        std::env::args().nth(1).as_deref() == Some("--native-gui"),
        "Pass --native-gui to opt into opening owned test windows"
    );
    let (completed, outcome) = std::sync::mpsc::channel();
    Application::new().with_assets(icons::Icons).run(move |cx| {
        cx.spawn(async move |cx| {
            let result = exercise(cx).await;
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A closed acceptance result lane has no caller; quitting still disposes all owned windows"
            )]
            let _ = completed.send(result);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A disposed app has already ended the native acceptance loop"
            )]
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
    outcome
        .try_recv()
        .context("Native rendering check did not complete")?
}

#[expect(
    clippy::future_not_send,
    reason = "Owned-window acceptance resolves and mutates native GPUI windows only on their UI thread"
)]
async fn exercise(cx: &AsyncApp) -> anyhow::Result<()> {
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.0), px(0.0)),
                size(px(64.0), px(64.0)),
            ))),
            titlebar: None,
            kind: WindowKind::PopUp,
            focus: false,
            show: false,
            ..Default::default()
        },
        |_, cx| cx.new(|_| Scene::default()),
    )?;
    let raw = cx.update(|cx| {
        window.update(cx, |_, window, _| {
            HasWindowHandle::window_handle(window)
                .map(|handle| handle.as_raw())
                .map_err(|error| anyhow::anyhow!("Native window handle: {error}"))
        })?
    })??;
    speakeasy_platform::configure_pill(raw)?;
    for iteration in 0..CYCLES {
        // A pathless first cycle makes a later one exercise the renderer's lazy path targets.
        let draws_path = iteration != 0;
        cx.update(|cx| window.update(cx, |scene, _, _| scene.draws_path = draws_path))??;
        speakeasy_platform::set_pill_visible(raw, true);
        let preceding = window.read_with(cx, |scene, _| scene.renders)?;
        let width = if iteration % 2 == 0 { 64.0 } else { 96.0 };
        cx.update(|cx| {
            window.update(cx, |_, window, _| {
                window.resize(size(px(width), px(64.0)));
                window.refresh();
            })
        })??;
        wait_for_render(window, preceding, cx).await?;
        speakeasy_platform::set_pill_visible(raw, false);
        Timer::after(PRESENT_SETTLE).await;
    }
    println!(
        "PASS: native render callbacks completed across SVG/path scenes, resize, and {CYCLES} show/hide cycles."
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance reads the view through GPUI's thread-affine UI context between frame waits"
)]
async fn wait_for_render(
    window: WindowHandle<Scene>,
    preceding: usize,
    cx: &AsyncApp,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let deadline = started.checked_add(TIMEOUT).unwrap_or(started);
    while window.read_with(cx, |scene, _| scene.renders)? == preceding {
        Timer::after(POLL_INTERVAL).await;
        ensure!(
            Instant::now() < deadline,
            "Native frame source did not redraw"
        );
    }
    Timer::after(PRESENT_SETTLE).await;
    Ok(())
}

fn paint_diamond(bounds: Bounds<Pixels>, window: &mut Window) {
    let vertex = |x: f32, y: f32| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
    let mut path = PathBuilder::fill();
    path.move_to(vertex(24.0, 18.0));
    path.line_to(vertex(30.0, 24.0));
    path.line_to(vertex(24.0, 30.0));
    path.line_to(vertex(18.0, 24.0));
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, rgb(0xFF_77_33));
    }
}
