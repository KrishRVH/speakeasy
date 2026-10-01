//! Opt-in own-window rendering and frame-source acceptance, without audio,
//! global input or clipboard access.

use anyhow::{Context as _, ensure};
use gpui::{
    AppContext, Application, AsyncApp, Bounds, PathBuilder, Pixels, Timer, Window, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, canvas, div, point, prelude::*, px, rgb, size, svg,
};
use raw_window_handle::HasWindowHandle;
use std::time::{Duration, Instant};

#[path = "../src/icons.rs"]
mod icons;

#[derive(Default)]
struct Colors {
    paths: bool,
    renders: usize,
}

impl Render for Colors {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        self.renders = self.renders.saturating_add(1);
        let paths = self.paths;
        div()
            .size_full()
            .bg(rgb(0x11_33_55))
            .child(div().w(px(32.0)).h_full().bg(rgb(0x55_77_99)))
            .child(
                svg()
                    .path(icons::KEYSTONE)
                    .absolute()
                    .top(px(8.0))
                    .left(px(8.0))
                    .size(px(10.0))
                    .text_color(rgb(0xcc_99_55)),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        if paths {
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

fn paint_diamond(bounds: Bounds<Pixels>, window: &mut Window) {
    let mut path = PathBuilder::fill();
    for (index, (x, y)) in [(24.0, 18.0), (30.0, 24.0), (24.0, 30.0), (18.0, 24.0)]
        .into_iter()
        .enumerate()
    {
        let vertex = point(bounds.origin.x + px(x), bounds.origin.y + px(y));
        if index == 0 {
            path.move_to(vertex);
        } else {
            path.line_to(vertex);
        }
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, rgb(0xff_77_33));
    }
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance reads the view through GPUI's thread-affine UI context between frame waits"
)]
async fn wait_for_render(
    window: WindowHandle<Colors>,
    preceding: usize,
    cx: &AsyncApp,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let deadline = started
        .checked_add(Duration::from_secs(3))
        .unwrap_or(started);
    while window.read_with(cx, |colors, _| colors.renders)? == preceding {
        Timer::after(Duration::from_millis(20)).await;
        ensure!(
            Instant::now() < deadline,
            "Native frame source did not redraw"
        );
    }
    // Allow the native renderer to finish submitting this frame.
    Timer::after(Duration::from_millis(20)).await;
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_pixels(window: u32, scale: f32, paths: bool) -> anyhow::Result<()> {
    use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
    let (connection, _) = x11rb::connect(None)?;
    let geometry = connection.get_geometry(window)?.reply()?;
    let image = connection
        .get_image(
            ImageFormat::Z_PIXMAP,
            window,
            0,
            0,
            geometry.width,
            geometry.height,
            u32::MAX,
        )?
        .reply()?;
    let pixels = image.data.as_chunks::<4>().0;
    let color = |x: u16, y: u16| -> anyhow::Result<u32> {
        let column = pixel_coordinate(x, scale)?;
        let row = pixel_coordinate(y, scale)?;
        ensure!(
            column < usize::from(geometry.width) && row < usize::from(geometry.height),
            "Sample coordinate is outside the owned window"
        );
        let index = row
            .checked_mul(usize::from(geometry.width))
            .and_then(|offset| offset.checked_add(column))
            .context("Native image geometry is unsupported")?;
        let pixel = pixels
            .get(index)
            .context("Native image is shorter than its geometry")?;
        Ok(u32::from_ne_bytes(*pixel) & 0x00ff_ffff)
    };
    ensure!(color(13, 13)? == 0xcc_99_55, "Embedded SVG did not render");
    ensure!(
        color(50, 50)? == 0x11_33_55,
        "Window background did not render"
    );
    if paths {
        ensure!(
            color(24, 24)? == 0xff_77_33,
            "Path did not render after allocation or resize"
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn wait_for_pixels(window: u32, scale: f32, paths: bool) -> anyhow::Result<()> {
    let started = Instant::now();
    let deadline = started
        .checked_add(Duration::from_secs(3))
        .unwrap_or(started);
    loop {
        match verify_pixels(window, scale, paths) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(error);
                }
                Timer::after(Duration::from_millis(20)).await;
            },
        }
    }
}

#[cfg(target_os = "linux")]
fn pixel_coordinate(logical: u16, scale: f32) -> anyhow::Result<usize> {
    let physical = f32::from(logical) * scale;
    ensure!(
        scale.is_finite() && scale > 0.0 && (0.0..=f32::from(u16::MAX)).contains(&physical),
        "Native display scale is unsupported"
    );
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Sampling intentionally rounds a validated native coordinate down to its containing pixel"
    )]
    #[expect(
        clippy::cast_sign_loss,
        reason = "The physical coordinate is validated as finite and nonnegative before conversion"
    )]
    Ok(physical as usize)
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
        |_, cx| cx.new(|_| Colors::default()),
    )?;
    let (raw, scale) = cx.update(|cx| {
        window.update(cx, |_, window, _| {
            HasWindowHandle::window_handle(window)
                .map(|handle| (handle.as_raw(), window.scale_factor()))
                .map_err(|error| anyhow::anyhow!("Native window handle: {error}"))
        })?
    })??;
    #[cfg(not(target_os = "linux"))]
    let _ = scale;
    speakeasy_platform::configure_pill(raw)?;
    #[cfg(target_os = "linux")]
    let xcb = match raw {
        raw_window_handle::RawWindowHandle::Xcb(handle) => handle.window.get(),
        _ => anyhow::bail!("Expected an XCB window on the private display"),
    };
    for iteration in 0..25 {
        let paths = iteration != 0;
        cx.update(|cx| window.update(cx, |colors, _, _| colors.paths = paths))??;
        speakeasy_platform::set_pill_visible(raw, true);
        let preceding = window.read_with(cx, |colors, _| colors.renders)?;
        cx.update(|cx| {
            window.update(cx, |_, window, _| {
                let width = if iteration % 2 == 0 { 64.0 } else { 96.0 };
                window.resize(size(px(width), px(64.0)));
                window.refresh();
            })
        })??;
        wait_for_render(window, preceding, cx).await?;
        #[cfg(target_os = "linux")]
        wait_for_pixels(xcb, scale, paths).await?;
        speakeasy_platform::set_pill_visible(raw, false);
        Timer::after(Duration::from_millis(20)).await;
    }
    #[cfg(target_os = "linux")]
    println!("PASS: SVG and path pixels verified across resize and 25 native show/hide cycles.");
    #[cfg(not(target_os = "linux"))]
    println!(
        "PASS: native render callbacks completed across SVG/path scenes, resize, and 25 show/hide cycles."
    );
    Ok(())
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
        }).detach();
    });
    outcome
        .try_recv()
        .context("Native rendering check did not complete")?
}
