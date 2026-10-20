//! The overlay: a click-through window over the display a Show records, where ink appears as it
//! is drawn and fades soon after the pen lifts. It never takes input or focus, and it is left out
//! of every frame, so the bundle redraws the same ink cleanly.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, FillOptions, FillRule, PathBuilder, PathStyle,
    Render, Rgba, Task, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, canvas, div, point, prelude::*, px,
};
use raw_window_handle::RawWindowHandle;
use speakeasy_platform::Display;
use speakeasy_show::{Point, ink};
use tokio::sync::watch;

use crate::gpui_ext::raw_handle;

/// Ink holds this long after the pen last lifts, then fades over [`FADE`], so a circle and the
/// arrow drawn right after it stay and leave together.
const HOLD: Duration = Duration::from_millis(650);
const FADE: Duration = Duration::from_millis(650);
/// The red of ink, which the bundle draws in the same color.
const INK: Rgba = Rgba {
    r: 1.0,
    g: 59.0 / 255.0,
    b: 48.0 / 255.0,
    a: 1.0,
};

/// What the overlay shows, published by a Show's recorder.
#[derive(Clone, Debug, Default)]
pub(crate) struct Canvas {
    /// The display to cover; `None` hides the overlay.
    pub display: Option<Display>,
    /// Finished strokes, normalized to the display.
    pub strokes: Vec<Vec<Point>>,
    /// The stroke being drawn.
    pub drawing: Vec<Point>,
    /// When the pen last lifted.
    pub lifted: Option<Instant>,
}

impl Canvas {
    /// How opaque the ink is at `now`: whole while drawing and through [`HOLD`], then easing out.
    pub(crate) fn opacity(&self, now: Instant) -> f32 {
        match self.lifted {
            Some(lifted) if self.drawing.is_empty() => {
                let fading = now
                    .saturating_duration_since(lifted)
                    .saturating_sub(HOLD)
                    .div_duration_f32(FADE)
                    .min(1.0);
                (1.0 - fading).powi(2)
            },
            _ => 1.0,
        }
    }

    /// Whether every stroke has faded away, so the next one can start a clean canvas.
    pub(crate) fn faded(&self, now: Instant) -> bool {
        self.opacity(now) <= 0.0
    }
}

struct Overlay {
    canvas: watch::Receiver<Canvas>,
    window: AnyWindowHandle,
    display: Option<Display>,
    visibility: Option<Task<()>>,
    _updates: Task<()>,
}

impl Overlay {
    fn new(canvas: watch::Receiver<Canvas>, window: &Window, cx: &Context<Self>) -> Self {
        Self {
            _updates: Self::follow(canvas.clone(), window, cx),
            canvas,
            window: window.window_handle(),
            display: None,
            visibility: None,
        }
    }

    /// Redraws on every canvas change while the window and publisher live.
    fn follow(
        mut canvas: watch::Receiver<Canvas>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            while canvas.changed().await.is_ok() {
                let display = canvas.borrow_and_update().display;
                if this
                    .update_in(cx, |overlay, _, cx| overlay.cover(display, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// Moves onto `display` and shows, or hides when there is none. Native calls run in a task,
    /// after the window borrow ends, because showing can synchronously request a frame.
    fn cover(&mut self, display: Option<Display>, cx: &mut Context<Self>) {
        if display != self.display {
            self.display = display;
            let window = self.window;
            self.visibility = Some(cx.spawn(async move |_, cx| {
                if let Ok(Ok(raw)) =
                    cx.update_window(window, |_, window, _| raw_handle(window, "overlay"))
                {
                    place(raw, display);
                }
            }));
        }
        cx.notify();
    }
}

/// Covers `display` and shows the overlay, or hides it. An overlay that cannot cover the display
/// stays hidden rather than misplaced.
fn place(raw: RawWindowHandle, display: Option<Display>) {
    let covered =
        display.is_some_and(|display| speakeasy_platform::configure_overlay(raw, display).is_ok());
    speakeasy_platform::set_overlay_visible(raw, covered);
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let ink = self.canvas.borrow().clone();
        let opacity = ink.opacity(now);
        if opacity > 0.0 && ink.lifted.is_some() && ink.drawing.is_empty() {
            window.request_animation_frame();
        }
        div().size_full().child(
            canvas(
                |_, _, _| {},
                move |bounds, (), window, _| {
                    if opacity > 0.0 {
                        paint(&ink, bounds, opacity, window);
                    }
                },
            )
            .size_full(),
        )
    }
}

/// Paints every stroke as the pen outline the bundle draws, at a width that reads on screen.
fn paint(canvas: &Canvas, bounds: Bounds<gpui::Pixels>, opacity: f32, window: &mut Window) {
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let pen = ink::pen(width, height);
    let color = Rgba { a: opacity, ..INK };
    for stroke in canvas
        .strokes
        .iter()
        .chain(std::iter::once(&canvas.drawing))
    {
        let pixels: Vec<Point> = stroke
            .iter()
            .map(|point| point.scaled(width, height))
            .collect();
        let outline = ink::outline(&pixels, pen);
        // Winding, as the bundle fills it: where a stroke crosses itself the ink stays solid.
        let mut path = PathBuilder::fill().with_style(PathStyle::Fill(
            FillOptions::default().with_fill_rule(FillRule::NonZero),
        ));
        let mut corners = outline.iter().map(|corner| {
            point(
                bounds.origin.x + px(corner.x),
                bounds.origin.y + px(corner.y),
            )
        });
        let Some(first) = corners.next() else {
            continue;
        };
        path.move_to(first);
        for corner in corners {
            path.line_to(corner);
        }
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}

/// Opens the overlay whenever a Show needs it and closes it once the Show ends: a full-screen
/// window holds GPU memory the size of its display, which an idle app has no use for. A platform
/// that records no Shows never opens one.
pub(crate) fn attend(mut canvas: watch::Receiver<Canvas>, cx: &App) {
    cx.spawn(async move |cx| {
        loop {
            if canvas
                .wait_for(|canvas| canvas.display.is_some())
                .await
                .is_err()
            {
                return;
            }
            let overlay = match cx.update(|cx| open(canvas.clone(), cx)) {
                Ok(Ok(overlay)) => overlay,
                Ok(Err(error)) => {
                    speakeasy_platform::show_error(&format!(
                        "Cannot open the Show overlay; ink will not appear while drawing: {error:#}"
                    ));
                    return;
                },
                Err(_) => return,
            };
            let published = canvas
                .wait_for(|canvas| canvas.display.is_none())
                .await
                .is_ok();
            let closed = overlay
                .update(cx, |_, window, _| window.remove_window())
                .is_ok();
            if !(published && closed) {
                return;
            }
        }
    })
    .detach();
}

fn open(
    mut canvas: watch::Receiver<Canvas>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Overlay>> {
    let display = cx.primary_display().context("No display available")?;
    // The overlay first covers the display already waiting in the canvas.
    canvas.mark_changed();
    let overlay = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(display.bounds())),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            window.set_window_title("Speakeasy overlay");
            cx.new(|cx| Overlay::new(canvas, window, cx))
        },
    )?;
    Ok(overlay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_holds_while_drawing_then_fades_out_after_the_pen_lifts() {
        let lifted = Instant::now();
        let mut canvas = Canvas {
            lifted: Some(lifted),
            ..Canvas::default()
        };
        let at = |millis| lifted.checked_add(Duration::from_millis(millis)).unwrap();
        assert!((canvas.opacity(at(600)) - 1.0).abs() < f32::EPSILON);
        let midway = canvas.opacity(at(975));
        assert!(midway > 0.2 && midway < 0.3, "{midway}");
        assert!(canvas.faded(at(1_300)));
        canvas.drawing.push(Point::default());
        assert!((canvas.opacity(at(5_000)) - 1.0).abs() < f32::EPSILON);
    }
}
