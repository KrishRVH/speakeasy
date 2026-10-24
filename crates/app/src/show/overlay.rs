//! The overlay: a click-through window over the display a Show records, where ink appears as it
//! is drawn and fades soon after the pen lifts. It never takes input or focus, and it is left out
//! of every frame, so the bundle redraws the same ink cleanly.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow};
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, FillOptions, FillRule, PathBuilder, PathStyle,
    Render, Rgba, Task, Timer, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, canvas, div, point, prelude::*, px,
};
use speakeasy_platform::{Display, NativeWindow};
use speakeasy_show::{Point, ink};
use tokio::sync::watch;

use crate::{
    gpui_ext::{EntityUpdate, WindowUpdate, defer_native, follow},
    runtime::{Phase, Snapshot},
};

/// Ink holds this long after the pen last lifts, then fades over [`FADE`], so a circle and the
/// arrow drawn right after it stay and leave together.
const HOLD: Duration = Duration::from_millis(650);
const FADE: Duration = Duration::from_millis(650);
const INK: Rgba = {
    let [red, green, blue, _] = ink::COLOR;
    Rgba {
        r: red as f32 / 255.0,
        g: green as f32 / 255.0,
        b: blue as f32 / 255.0,
        a: 1.0,
    }
};

/// What the overlay shows, published by a Show's recorder or the demo's.
#[derive(Clone, Debug, Default)]
pub(crate) struct Canvas {
    /// The display to cover; `None` hides the overlay.
    display: Option<Display>,
    /// Finished strokes, normalized to the display. Each is an immutable snapshot, so a render's
    /// copy of the canvas copies only the stroke being drawn.
    strokes: Vec<Arc<[Point]>>,
    /// The stroke being drawn.
    drawing: Vec<Point>,
    /// When the pen last lifted.
    lifted: Option<Instant>,
}

impl Canvas {
    /// A clean canvas over `display`.
    pub(crate) fn covering(display: Display) -> Self {
        Self {
            display: Some(display),
            ..Self::default()
        }
    }

    /// The display to cover; `None` hides the overlay.
    pub(crate) const fn display(&self) -> Option<Display> {
        self.display
    }

    /// Keeps the `finished` strokes and shows `drawing` as the stroke in progress, both normalized
    /// to the display, at `now`. Ink arriving after earlier ink has faded starts a clean canvas,
    /// and ink no longer being drawn starts the hold and fade; a stroke cut for length while the
    /// pen keeps drawing does neither. Returns whether anything visible changed.
    pub(crate) fn update(
        &mut self,
        finished: impl IntoIterator<Item = Arc<[Point]>>,
        drawing: impl IntoIterator<Item = Point>,
        now: Instant,
    ) -> bool {
        let finished: Vec<Arc<[Point]>> = finished.into_iter().collect();
        let drawing: Vec<Point> = drawing.into_iter().collect();
        let was_drawing = !self.drawing.is_empty();
        let inked = !finished.is_empty() || !drawing.is_empty();
        if inked && self.faded(now) {
            self.strokes.clear();
        }
        let changed = !finished.is_empty() || drawing != self.drawing;
        if drawing.is_empty() && (was_drawing || !finished.is_empty()) {
            self.lifted = Some(now);
        }
        self.strokes.extend(finished);
        self.drawing = drawing;
        changed
    }

    /// How long ago the pen lifted, while no stroke is being drawn.
    fn since_lift(&self, now: Instant) -> Option<Duration> {
        self.lifted
            .filter(|_| self.drawing.is_empty())
            .map(|lifted| now.saturating_duration_since(lifted))
    }

    /// How opaque the ink is at `now`: whole while drawing and through [`HOLD`], then easing out.
    fn opacity(&self, now: Instant) -> f32 {
        self.since_lift(now).map_or(1.0, |since| {
            let fading = since.saturating_sub(HOLD).div_duration_f32(FADE).min(1.0);
            (1.0 - fading).powi(2)
        })
    }

    /// Whether every stroke has faded away.
    fn faded(&self, now: Instant) -> bool {
        self.opacity(now) <= 0.0
    }

    /// When the overlay next needs a frame at `now`, if the canvas does not change first.
    fn next_frame(&self, now: Instant) -> Redraw {
        match self.since_lift(now) {
            Some(since) if since < HOLD => Redraw::After(HOLD.saturating_sub(since)),
            Some(_) if self.opacity(now) > 0.0 => Redraw::Animate,
            _ => Redraw::OnChange,
        }
    }
}

/// When the overlay draws again, beyond the frame every canvas change brings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Redraw {
    /// Only on a change: the pen is drawing, has not lifted, or its ink has faded.
    OnChange,
    /// Once, after this long, when the hold ends and the fade begins.
    After(Duration),
    /// Every frame while the ink fades.
    Animate,
}

struct Overlay {
    canvas: watch::Receiver<Canvas>,
    window: AnyWindowHandle,
    display: Option<Display>,
    visibility: Option<Task<()>>,
    /// Redraws once the hold ends, so held ink costs no frames.
    wake: Option<Task<()>>,
    _updates: Task<()>,
}

impl Overlay {
    fn new(canvas: watch::Receiver<Canvas>, window: &Window, cx: &Context<Self>) -> Self {
        Self {
            _updates: follow(canvas.clone(), cx, Canvas::display, Self::cover),
            canvas,
            window: window.window_handle(),
            display: None,
            visibility: None,
            wake: None,
        }
    }

    /// Moves onto `display` and shows, or hides when there is none, then redraws the ink.
    fn cover(&mut self, display: Option<Display>, cx: &mut Context<Self>) {
        if display != self.display {
            self.display = display;
            self.visibility = Some(defer_native(
                self.window,
                "overlay",
                cx,
                move |native, _| place(native, display),
            ));
        }
        cx.notify();
    }
}

/// Covers `display` and shows the overlay, or hides it. An overlay that cannot cover the display
/// stays hidden rather than misplaced.
fn place(native: &NativeWindow, display: Option<Display>) {
    let covered = display.is_some_and(|display| native.configure_overlay(display).is_ok());
    native.set_overlay_visible(covered);
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let ink = self.canvas.borrow().clone();
        let opacity = ink.opacity(now);
        self.wake = match ink.next_frame(now) {
            Redraw::OnChange => None,
            // A wake that fires early renders once and schedules the rest of the hold.
            Redraw::After(delay) => Some(cx.spawn(async move |this, cx| {
                Timer::after(delay).await;
                this.update_if_alive(cx, |_, cx| cx.notify());
            })),
            Redraw::Animate => {
                window.request_animation_frame();
                None
            },
        };
        div().size_full().child(
            canvas(
                |_, _, _| {},
                move |bounds, (), window, _| {
                    // The first frame after the fade paints no path, which lets the renderer free
                    // its full-window path targets.
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
        .map(Arc::as_ref)
        .chain(std::iter::once(canvas.drawing.as_slice()))
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
///
/// The first overlay that cannot open goes to `report` once its Show is copied, which the
/// failure would otherwise leave looking complete. A Show that fails or is cancelled says so
/// itself, so a later Show reports instead.
pub(crate) fn attend(
    mut canvas: watch::Receiver<Canvas>,
    mut snapshots: watch::Receiver<Snapshot>,
    report: impl Fn(&anyhow::Error, &mut App) + 'static,
    cx: &App,
) {
    cx.spawn(async move |cx| {
        let mut reported = false;
        loop {
            if canvas
                .wait_for(|canvas| canvas.display.is_some())
                .await
                .is_err()
            {
                return;
            }
            let show = snapshots.borrow().id;
            let (overlay, failure) = match cx.update(|cx| open(canvas.clone(), cx)) {
                Ok(Ok(overlay)) => (Some(overlay), None),
                Ok(Err(error)) => (None, (!reported).then_some(error)),
                Err(_) => return,
            };
            let released = canvas
                .wait_for(|canvas| canvas.display.is_none())
                .await
                .is_ok();
            if let Some(overlay) = overlay {
                overlay.update_if_open(cx, |_, window, _| window.remove_window());
            }
            if !released {
                return;
            }
            let Some(error) = failure else {
                continue;
            };
            // The next session cannot start until this one leaves the active phases, so waiting
            // here never delays the next Show's overlay. A new id means pause, quit or a newer
            // session took the snapshot first.
            let Ok(ended) = snapshots
                .wait_for(|snapshot| snapshot.id != show || !snapshot.phase.is_active())
                .await
                .map(|snapshot| snapshot.id == show && snapshot.phase == Phase::Copied)
            else {
                return;
            };
            if ended {
                reported = true;
                let error =
                    anyhow!("Show copied, but its ink could not appear on screen: {error:#}");
                if cx.update(|cx| report(&error, cx)).is_err() {
                    return;
                }
            }
        }
    })
    .detach();
}

fn open(canvas: watch::Receiver<Canvas>, cx: &mut App) -> anyhow::Result<WindowHandle<Overlay>> {
    let display = cx.primary_display().context("No display available")?;
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

    /// A distinct stroke of `points` points across the display.
    fn stroke(points: u8) -> Vec<Point> {
        (0..points)
            .map(|point| Point::new(f32::from(point) / 100.0, 0.5))
            .collect()
    }

    /// A clock reading `millis` after a moment fixed per test.
    fn clock() -> impl Fn(u64) -> Instant {
        let start = Instant::now();
        move |millis| start.checked_add(Duration::from_millis(millis)).unwrap()
    }

    fn whole(canvas: &Canvas, now: Instant) -> bool {
        (canvas.opacity(now) - 1.0).abs() < f32::EPSILON
    }

    #[test]
    fn ink_holds_while_drawing_then_fades_out_after_the_pen_lifts() {
        let at = clock();
        let mut canvas = Canvas::default();
        assert!(canvas.update([], stroke(3), at(0)));
        assert!(canvas.update([stroke(3).into()], [], at(0)));
        assert!(whole(&canvas, at(600)));
        let midway = canvas.opacity(at(975));
        assert!(midway > 0.2 && midway < 0.3, "{midway}");
        assert!(canvas.faded(at(1_300)));
        // Drawing again keeps every mark whole for as long as the stroke takes.
        assert!(canvas.update([], stroke(4), at(600)));
        assert!(whole(&canvas, at(5_000)));
    }

    #[test]
    fn held_ink_draws_no_frames_until_it_fades() {
        let at = clock();
        let mut canvas = Canvas::default();
        canvas.update([], stroke(3), at(0));
        assert_eq!(canvas.next_frame(at(0)), Redraw::OnChange);
        canvas.update([stroke(3).into()], [], at(0));
        let left = Duration::from_millis(50);
        assert_eq!(canvas.next_frame(at(600)), Redraw::After(left));
        assert_eq!(canvas.next_frame(at(975)), Redraw::Animate);
        assert_eq!(canvas.next_frame(at(1_300)), Redraw::OnChange);
    }

    #[test]
    fn marks_drawn_before_the_fade_ends_stay_and_later_ink_starts_clean() {
        let at = clock();
        let mut canvas = Canvas::default();
        canvas.update([], stroke(3), at(0));
        canvas.update([stroke(3).into()], [], at(100));
        // An arrow drawn while the circle fades keeps it.
        canvas.update([], stroke(4), at(1_000));
        canvas.update([stroke(4).into()], [], at(1_100));
        assert_eq!(canvas.strokes.len(), 2);
        // A stroke cut for length while the pen keeps drawing stays whole.
        canvas.update([], stroke(5), at(1_150));
        assert!(canvas.update([stroke(5).into()], stroke(2), at(1_200)));
        assert!(whole(&canvas, at(9_000)));
        assert_eq!(canvas.strokes.len(), 3);
        // A pointer that moved too little to add ink changes nothing.
        assert!(!canvas.update([], stroke(2), at(1_300)));
        canvas.update([stroke(2).into()], [], at(1_400));
        assert_eq!(canvas.next_frame(at(1_400)), Redraw::After(HOLD));
        // Ink once everything has faded starts a clean canvas.
        assert!(canvas.update([], stroke(3), at(2_800)));
        assert_eq!(canvas.strokes, Vec::<Arc<[Point]>>::new());
        assert!(!Canvas::default().update([], [], at(0)));
    }
}
