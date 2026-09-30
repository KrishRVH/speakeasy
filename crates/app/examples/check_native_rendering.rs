//! Opt-in own-window rendering and frame-source check. No audio, input or clipboard.

use anyhow::{Context as _, ensure};
use gpui::{prelude::*, *};
use raw_window_handle::HasWindowHandle;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

#[path = "../src/icons.rs"]
mod icons;

struct Colors {
    paths: Rc<Cell<bool>>,
    renders: Rc<Cell<usize>>,
}

impl Render for Colors {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let paths = self.paths.get();
        div()
            .size_full()
            .bg(rgb(0x113355))
            .child(div().w(px(32.0)).h_full().bg(rgb(0x557799)))
            .child(
                svg()
                    .path(icons::KEYSTONE)
                    .absolute()
                    .top(px(8.0))
                    .left(px(8.0))
                    .size(px(10.0))
                    .text_color(rgb(0xcc9955)),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        if paths {
                            let mut path = PathBuilder::fill();
                            for (index, (x, y)) in
                                [(24.0, 18.0), (30.0, 24.0), (24.0, 30.0), (18.0, 24.0)]
                                    .into_iter()
                                    .enumerate()
                            {
                                let point = bounds.origin + point(px(x), px(y));
                                if index == 0 {
                                    path.move_to(point);
                                } else {
                                    path.line_to(point);
                                }
                            }
                            path.close();
                            if let Ok(path) = path.build() {
                                window.paint_path(path, rgb(0xff7733));
                            }
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

async fn wait_for_render(renders: &Cell<usize>, preceding: usize) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while renders.get() == preceding {
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
fn verify_pixels(
    window: raw_window_handle::RawWindowHandle,
    scale: f32,
    paths: bool,
) -> anyhow::Result<()> {
    use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
    let raw_window_handle::RawWindowHandle::Xcb(window) = window else {
        anyhow::bail!("Expected an XCB window on the private display");
    };
    let (connection, _) = x11rb::connect(None)?;
    let width = (64.0 * scale).ceil() as u16;
    let image = connection
        .get_image(
            ImageFormat::Z_PIXMAP,
            window.window.get(),
            0,
            0,
            width,
            width,
            u32::MAX,
        )?
        .reply()?;
    let pixels = image.data.as_chunks::<4>().0;
    let color = |x: f32, y: f32| {
        u32::from_ne_bytes(pixels[(y * scale) as usize * width as usize + (x * scale) as usize])
            & 0x00ff_ffff
    };
    ensure!(color(13.0, 13.0) == 0xcc9955, "Embedded SVG did not render");
    ensure!(
        color(50.0, 50.0) == 0x113355,
        "Window background did not render"
    );
    if paths {
        ensure!(
            color(24.0, 24.0) == 0xff7733,
            "Path did not render after allocation or resize"
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn wait_for_pixels(
    window: raw_window_handle::RawWindowHandle,
    scale: f32,
    paths: bool,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match verify_pixels(window, scale, paths) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(error);
                }
                Timer::after(Duration::from_millis(20)).await;
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    ensure!(
        std::env::args().nth(1).as_deref() == Some("--native-gui"),
        "Pass --native-gui to opt into opening owned test windows"
    );
    let outcome = Rc::new(RefCell::new(None));
    let completed = outcome.clone();
    Application::new().with_assets(icons::Icons).run(move |cx| {
        cx.spawn(async move |cx| {
            let result = async {
                let renders = Rc::new(Cell::new(0));
                let paths = Rc::new(Cell::new(false));
                let window = cx.update(|cx| {
                    cx.open_window(
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
                        |_, cx| {
                            cx.new(|_| Colors {
                                paths: paths.clone(),
                                renders: renders.clone(),
                            })
                        },
                    )
                })??;
                let (raw, _scale) = cx.update(|cx| {
                    window.update(cx, |_, window, _| {
                        HasWindowHandle::window_handle(window)
                            .map(|handle| (handle.as_raw(), window.scale_factor()))
                            .map_err(|error| anyhow::anyhow!("Native window handle: {error}"))
                    })?
                })??;
                speakeasy_platform::configure_pill(raw)?;
                for iteration in 0..25 {
                    paths.set(iteration != 0);
                    speakeasy_platform::set_pill_visible(raw, true);
                    let preceding = renders.get();
                    cx.update(|cx| {
                        window.update(cx, |_, window, _| {
                            window.resize(size(px(64.0 + (iteration % 2) as f32 * 32.0), px(64.0)));
                            window.refresh();
                        })
                    })??;
                    wait_for_render(&renders, preceding).await?;
                    #[cfg(target_os = "linux")]
                    wait_for_pixels(raw, _scale, paths.get()).await?;
                    speakeasy_platform::set_pill_visible(raw, false);
                    Timer::after(Duration::from_millis(20)).await;
                }
                #[cfg(target_os = "linux")]
                println!(
                    "PASS: SVG and path pixels verified across resize and 25 native show/hide cycles."
                );
                #[cfg(not(target_os = "linux"))]
                println!(
                    "PASS: native render callbacks completed across SVG/path scenes, resize, and 25 show/hide cycles."
                );
                Ok(())
            }
            .await;
            *completed.borrow_mut() = Some(result);
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
    let result = outcome.borrow_mut().take();
    result.context("Native rendering check did not complete")?
}
