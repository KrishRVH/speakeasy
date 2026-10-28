//! Opt-in owned-window rendering and frame-source acceptance, without audio, global input or
//! clipboard access.

#![forbid(unsafe_code)]

#[path = "../src/icons.rs"]
mod icons;

use std::{
    io::{self, Write as _},
    time::{Duration, Instant},
};

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

const BACKGROUND: u32 = 0x11_33_55;
const ICON: u32 = 0xCC_99_55;
const DIAMOND: u32 = 0xFF_77_33;

#[derive(Default)]
struct Scene {
    draws_path: bool,
    renders: usize,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<'_, Self>) -> impl IntoElement {
        self.renders = self.renders.saturating_add(1);
        let draws_path = self.draws_path;
        div()
            .size_full()
            .bg(rgb(BACKGROUND))
            .child(
                svg()
                    .path(icons::KEYSTONE)
                    .absolute()
                    .top(px(8.0))
                    .left(px(8.0))
                    .size(px(10.0))
                    .text_color(rgb(ICON)),
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
                .top_0()
                .left_0()
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
async fn exercise(cx: &mut AsyncApp) -> anyhow::Result<()> {
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
    let native = window.update(cx, |_, window, _| {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow::anyhow!("Native window handle: {error}"))?;
        speakeasy_platform::NativeWindow::new(handle)
    })??;
    native.configure_pill()?;
    #[cfg(target_os = "linux")]
    let sampler = window.update(cx, |_, window, _| linux::Sampler::of(window))??;
    for iteration in 0..CYCLES {
        // A pathless first cycle makes a later one exercise the renderer's lazy path targets.
        let draws_path = iteration != 0;
        window.update(cx, |scene, _, _| scene.draws_path = draws_path)?;
        native.set_pill_visible(true);
        let preceding = window.read_with(cx, |scene, _| scene.renders)?;
        let width = if iteration % 2 == 0 { 64.0 } else { 96.0 };
        window.update(cx, |_, window, _| {
            window.resize(size(px(width), px(64.0)));
            window.refresh();
        })?;
        wait_for_render(window, preceding, cx).await?;
        #[cfg(target_os = "linux")]
        sampler.wait_for_pixels(draws_path).await?;
        native.set_pill_visible(false);
        Timer::after(PRESENT_SETTLE).await;
    }
    let mut out = io::stdout().lock();
    if cfg!(target_os = "linux") {
        writeln!(
            out,
            "PASS: SVG and path pixels verified across resize and {CYCLES} native show/hide cycles."
        )?;
    } else {
        writeln!(
            out,
            "PASS: native render callbacks completed across SVG/path scenes, resize, and {CYCLES} show/hide cycles."
        )?;
    }
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
    while window.read_with(cx, |scene, _| scene.renders)? == preceding {
        Timer::after(POLL_INTERVAL).await;
        ensure!(
            started.elapsed() < TIMEOUT,
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
        window.paint_path(path, rgb(DIAMOND));
    }
}

#[cfg(target_os = "linux")]
mod linux {
    //! Pixel samples of the owned window on the private X11 display.

    use std::time::Instant;

    use anyhow::{Context as _, anyhow, bail, ensure};
    use gpui::{Timer, Window};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use x11rb::{
        protocol::xproto::{ConnectionExt as _, ImageFormat},
        xcb_ffi::XCBConnection,
    };

    use super::{BACKGROUND, DIAMOND, ICON, POLL_INTERVAL, TIMEOUT};

    /// The owned window's id and display scale on the private X11 display.
    pub(super) struct Sampler {
        window: u32,
        scale: f32,
    }

    impl Sampler {
        pub(super) fn of(window: &Window) -> anyhow::Result<Self> {
            let handle = HasWindowHandle::window_handle(window)
                .map_err(|error| anyhow!("Native window handle: {error}"))?;
            let RawWindowHandle::Xcb(xcb) = handle.as_raw() else {
                bail!("Expected an XCB window on the private display");
            };
            Ok(Self {
                window: xcb.window.get(),
                scale: window.scale_factor(),
            })
        }

        pub(super) async fn wait_for_pixels(&self, draws_path: bool) -> anyhow::Result<()> {
            let (connection, _) = XCBConnection::connect(None)?;
            let started = Instant::now();
            loop {
                let verified = self.verify_pixels(&connection, draws_path);
                if verified.is_ok() || started.elapsed() >= TIMEOUT {
                    return verified;
                }
                Timer::after(POLL_INTERVAL).await;
            }
        }

        fn verify_pixels(
            &self,
            connection: &XCBConnection,
            draws_path: bool,
        ) -> anyhow::Result<()> {
            let geometry = connection.get_geometry(self.window)?.reply()?;
            let image = connection
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    self.window,
                    0,
                    0,
                    geometry.width,
                    geometry.height,
                    u32::MAX,
                )?
                .reply()?;
            let pixels = image.data.as_chunks::<4>().0;
            let color = |x: u16, y: u16| -> anyhow::Result<u32> {
                let column = pixel_coordinate(x, self.scale)?;
                let row = pixel_coordinate(y, self.scale)?;
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
                Ok(u32::from_ne_bytes(*pixel) & 0x00FF_FFFF)
            };
            ensure!(color(13, 13)? == ICON, "Embedded SVG did not render");
            ensure!(
                color(50, 50)? == BACKGROUND,
                "Window background did not render"
            );
            if draws_path {
                ensure!(
                    color(24, 24)? == DIAMOND,
                    "Path did not render after allocation or resize"
                );
            }
            Ok(())
        }
    }

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
}
