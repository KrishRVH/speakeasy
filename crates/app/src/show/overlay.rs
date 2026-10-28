//! The overlay: a window over the display a Show records, where ink and the numbers of marks
//! appear as they are made and fade soon after the pen lifts, and where an area Show's area is
//! dragged out. It never takes focus, takes input only while an area is being chosen, and is left
//! out of every frame, so the bundle redraws the same ink cleanly.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow};
use async_channel::Sender;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Div, FillOptions, FillRule, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, PathStyle, Pixels, Render, Rgba,
    Size, Task, Timer, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, canvas, div, point, prelude::*, px, relative, white,
};
use speakeasy_platform::{Display, NativeWindow, Part};
use speakeasy_show::{Mark, Point, Shape, ink};
use tokio::sync::watch;

use crate::{
    WORDMARK_FONT,
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
/// About how wide a digit of the brand face runs, in ems, so a label's digits fit its disc as the
/// bundle fits them.
const DIGIT_EM: f32 = 0.6;
/// The line just outside a window or area being recorded: the width `border_2` draws.
const FRAME: Pixels = px(2.0);
/// Shades the display while an area is chosen, leaving the area clear.
const SHADE: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.4,
};
/// The smallest area a drag chooses, in logical pixels each way; a shorter drag is a stray click.
const SMALLEST_AREA: f32 = 24.0;
const PICK_HINT: &str = "Drag over the area to record · Esc cancels";

/// What the overlay shows, published by a Show's recorder or the demo's.
#[derive(Clone, Debug)]
pub(crate) struct Canvas {
    /// The display to cover; `None` hides the overlay.
    display: Option<Display>,
    /// The part of the display recorded, which ink and marks are normalized to.
    part: Part,
    /// Set while the user drags out the area to record, which goes here.
    picking: Option<Sender<Part>>,
    /// Finished strokes. Each is an immutable snapshot, so a render's copy of the canvas copies
    /// only the stroke being drawn.
    strokes: Vec<Arc<[Point]>>,
    /// How many finished strokes were cleared before those in `strokes`: the first of them is the
    /// recording's stroke of this index.
    cleared: usize,
    /// The stroke being drawn.
    drawing: Vec<Point>,
    /// Every mark so far, numbered from one.
    marks: Arc<[Mark]>,
    /// The number of the first mark to show: the first made since the ink last faded, or the
    /// faded mark the ink on screen joined; it and later ones show.
    first: usize,
    /// When the pen last lifted, or a pin last dropped.
    lifted: Option<Instant>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self {
            display: None,
            part: Part::WHOLE,
            picking: None,
            strokes: Vec::new(),
            cleared: 0,
            drawing: Vec::new(),
            marks: Arc::new([]),
            first: 1,
            lifted: None,
        }
    }
}

impl Canvas {
    /// A clean canvas over `display`, for ink on its `part`.
    pub(crate) fn covering(display: Display, part: Part) -> Self {
        Self {
            display: Some(display),
            part,
            ..Self::default()
        }
    }

    /// A canvas over `display` on which the user drags out the part to record, which goes to
    /// `chosen`.
    pub(crate) fn picking(display: Display, chosen: Sender<Part>) -> Self {
        Self {
            display: Some(display),
            picking: Some(chosen),
            ..Self::default()
        }
    }

    /// The display to cover; `None` hides the overlay.
    pub(crate) const fn display(&self) -> Option<Display> {
        self.display
    }

    /// Moves the ink and marks with the part recorded, as its window moves. Returns whether it
    /// moved.
    pub(crate) fn track(&mut self, part: Part) -> bool {
        let moved = part != self.part;
        self.part = part;
        moved
    }

    /// Keeps the `finished` strokes, shows `drawing` as the stroke in progress, both normalized to
    /// the part recorded, and takes `marks` when they changed, at `now`. Every stroke the
    /// recording finishes arrives once, in order, and `marks` number them as the bundle does. Ink
    /// or a pin arriving after earlier ink has faded starts a clean canvas, and ink no longer being
    /// drawn or a pin starts the hold and fade; a stroke cut for length while the pen keeps
    /// drawing does neither. Returns whether anything visible changed.
    pub(crate) fn update(
        &mut self,
        finished: impl IntoIterator<Item = Arc<[Point]>>,
        drawing: impl IntoIterator<Item = Point>,
        marks: Option<Arc<[Mark]>>,
        now: Instant,
    ) -> bool {
        let finished: Vec<Arc<[Point]>> = finished.into_iter().collect();
        let drawing: Vec<Point> = drawing.into_iter().collect();
        let was_drawing = !self.drawing.is_empty();
        // A new mark is a finished stroke's or a pin's, which has no ink of its own.
        let marked = marks
            .as_ref()
            .is_some_and(|marks| marks.len() > self.marks.len());
        let inked = !finished.is_empty() || !drawing.is_empty() || marked;
        if inked && self.faded(now) {
            self.cleared = self.cleared.saturating_add(self.strokes.len());
            self.strokes.clear();
            self.first = self.marks.len().saturating_add(1);
        }
        let changed = !finished.is_empty() || drawing != self.drawing || marks.is_some();
        if drawing.is_empty() && (was_drawing || !finished.is_empty() || marked) {
            self.lifted = Some(now);
        }
        self.strokes.extend(finished);
        self.drawing = drawing;
        if let Some(marks) = marks {
            // Marks group strokes by when the pen went down, which can come before the ink faded
            // even when the pen drew nothing until after: the stroke then shows the number of the
            // faded mark it joined.
            if let Some(joined) = marks
                .iter()
                .find(|mark| mark.strokes.contains(&self.cleared))
            {
                self.first = self.first.min(joined.number);
            }
            self.marks = marks;
        }
        changed
    }

    /// The marks made since the ink last faded, whose numbers show.
    fn shown(&self) -> impl Iterator<Item = &Mark> {
        self.marks.iter().filter(|mark| mark.number >= self.first)
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
    /// The display covered, and whether an area is being chosen on it.
    cover: (Option<Display>, bool),
    /// Where a drag choosing an area began and where it is now, in window pixels.
    drag: Option<[gpui::Point<Pixels>; 2]>,
    visibility: Option<Task<()>>,
    /// Redraws once the hold ends, so held ink costs no frames.
    wake: Option<Task<()>>,
    _updates: Task<()>,
}

impl Overlay {
    fn new(canvas: watch::Receiver<Canvas>, window: &Window, cx: &Context<'_, Self>) -> Self {
        Self {
            _updates: follow(
                canvas.clone(),
                cx,
                |canvas| (canvas.display(), canvas.picking.is_some()),
                Self::cover,
            ),
            canvas,
            window: window.window_handle(),
            cover: (None, false),
            drag: None,
            visibility: None,
            wake: None,
        }
    }

    /// Moves onto the display and shows, taking input while an area is chosen, or hides when there
    /// is no display, then redraws. An area that cannot be chosen here is the whole display.
    fn cover(&mut self, cover: (Option<Display>, bool), cx: &mut Context<'_, Self>) {
        if cover != self.cover {
            self.cover = cover;
            let (display, picking) = cover;
            if !picking {
                self.drag = None;
            }
            let chosen = self.canvas.borrow().picking.clone();
            self.visibility = Some(defer_native(
                self.window,
                "overlay",
                cx,
                move |native, _| {
                    if !place(native, display, picking)
                        && let Some(chosen) = chosen
                    {
                        chosen.close();
                    }
                },
            ));
        }
        cx.notify();
    }

    /// Ends a drag at `to` on an overlay `size` across, sending the area it covers to the
    /// recorder. A chosen area stays clear until recording takes over; a stray click starts over.
    fn pick(&mut self, to: gpui::Point<Pixels>, size: Size<Pixels>, cx: &mut Context<'_, Self>) {
        let Some([from, _]) = self.drag else {
            return;
        };
        let sent = dragged(from, to, size).is_some_and(|part| {
            self.canvas
                .borrow()
                .picking
                .as_ref()
                .is_some_and(|chosen| chosen.try_send(part).is_ok())
        });
        self.drag = sent.then_some([from, to]);
        cx.notify();
    }

    /// The display shaded around the area being dragged out, with a hint until the drag begins.
    fn picker(&self, size: Size<Pixels>, cx: &Context<'_, Self>) -> Div {
        let hole = self
            .drag
            .map(|[from, to]| Bounds::from_corners(from.min(&to), from.max(&to)));
        let shade = |left: Pixels, top: Pixels, width: Pixels, height: Pixels| {
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(width)
                .h(height)
                .bg(SHADE)
        };
        let zero = px(0.0);
        let shades = match hole {
            None => vec![shade(zero, zero, size.width, size.height)],
            Some(hole) => vec![
                shade(zero, zero, size.width, hole.top()),
                shade(zero, hole.bottom(), size.width, size.height - hole.bottom()),
                shade(zero, hole.top(), hole.left(), hole.size.height),
                shade(
                    hole.right(),
                    hole.top(),
                    size.width - hole.right(),
                    hole.size.height,
                ),
            ],
        };
        div()
            .size_full()
            .cursor_crosshair()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.drag = Some([event.position; 2]);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let Some([_, to]) = &mut this.drag
                    && event.pressed_button == Some(MouseButton::Left)
                {
                    *to = event.position;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    this.pick(event.position, window.viewport_size(), cx);
                }),
            )
            .children(shades)
            .children(hole.map(frame))
            .when(hole.is_none(), |picker| picker.child(hint()))
    }
}

/// Covers `display` and shows the overlay, taking clicks only while `picking`, or hides it, and
/// returns whether it covers a display. An overlay that cannot cover the display stays hidden
/// rather than misplaced.
fn place(native: &NativeWindow, display: Option<Display>, picking: bool) -> bool {
    let covered = display.is_some_and(|display| native.configure_overlay(display).is_ok());
    native.set_overlay_input(covered && picking);
    native.set_overlay_visible(covered);
    covered
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let sheet = self.canvas.borrow().clone();
        let size = window.viewport_size();
        if sheet.picking.is_some() {
            self.wake = None;
            return self.picker(size, cx);
        }
        let now = Instant::now();
        let opacity = sheet.opacity(now);
        self.wake = match sheet.next_frame(now) {
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
        let area = on_overlay(sheet.part, size);
        let labels: Vec<Div> = if opacity > 0.0 {
            sheet.shown().map(|mark| label(mark, area)).collect()
        } else {
            Vec::new()
        };
        let framed = sheet.part != Part::WHOLE;
        div()
            .size_full()
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, _| {
                        // The first frame after the fade paints no path, which lets the renderer
                        // free its full-window path targets.
                        if opacity > 0.0 {
                            paint(&sheet, bounds, opacity, window);
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .when(framed, |overlay| overlay.child(frame(area)))
            .child(
                div()
                    .absolute()
                    .size_full()
                    .opacity(opacity)
                    .children(labels),
            )
    }
}

/// Where `part` of the display lies on an overlay `size` across.
fn on_overlay(part: Part, size: Size<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(size.width * part.left, size.height * part.top),
        gpui::size(size.width * part.width(), size.height * part.height()),
    )
}

/// The part of the display a drag from `from` to `to` covers on an overlay `size` across, or
/// `None` for a drag too short to mean an area.
fn dragged(from: gpui::Point<Pixels>, to: gpui::Point<Pixels>, size: Size<Pixels>) -> Option<Part> {
    let share = |point: gpui::Point<Pixels>| {
        [
            f32::from(point.x) / f32::from(size.width),
            f32::from(point.y) / f32::from(size.height),
        ]
    };
    let part = Part::between(share(from), share(to));
    let area = on_overlay(part, size).size;
    (f32::from(area.width) >= SMALLEST_AREA && f32::from(area.height) >= SMALLEST_AREA)
        .then_some(part)
}

/// A thin red line just outside `area`, the window or area being recorded or dragged out.
fn frame(area: Bounds<Pixels>) -> Div {
    div()
        .absolute()
        .left(area.left() - FRAME)
        .top(area.top() - FRAME)
        .w(area.size.width + FRAME * 2.0)
        .h(area.size.height + FRAME * 2.0)
        .border_2()
        .border_color(INK)
}

fn hint() -> Div {
    div()
        .absolute()
        .top(relative(0.12))
        .left_0()
        .w_full()
        .flex()
        .justify_center()
        .child(
            div()
                .px_4()
                .py_2()
                .rounded_lg()
                .bg(Rgba { a: 0.75, ..SHADE })
                .text_color(white())
                .text_size(px(15.0))
                .child(PICK_HINT),
        )
}

/// `mark`'s number in white on a red disc ringed in white beside what it numbers, with a dot at
/// the point a pin marks, as the bundle draws them; `area` is the part recorded.
fn label(mark: &Mark, area: Bounds<Pixels>) -> Div {
    let (width, height) = (f32::from(area.size.width), f32::from(area.size.height));
    let radius = ink::label_radius(width, height);
    let center = mark.label(width, height, radius);
    let digits = mark.number.to_string();
    let size = (ink::LABEL_TYPE * radius)
        .min(ink::LABEL_DIGITS * radius / (DIGIT_EM * digits.len() as f32));
    let number = disc(
        center,
        radius,
        radius * (1.0 - ink::LABEL_RING),
        Some(
            div()
                .font_family(WORDMARK_FONT)
                .text_size(px(size))
                .line_height(px(size))
                .text_color(white())
                .child(digits),
        ),
    );
    let pin = match mark.shape {
        Shape::Pin { point } => {
            let dot = ink::PIN_DOT * radius;
            Some(disc(
                point.scaled(width, height),
                dot,
                dot * (1.0 - 2.0 * ink::LABEL_RING),
                None,
            ))
        },
        _ => None,
    };
    div()
        .absolute()
        .left(area.left())
        .top(area.top())
        .children(pin)
        .child(number)
}

/// A white disc of `radius` centered at `center`, in pixels from the corner of what it labels,
/// holding a red one of radius `inner` around `content`.
fn disc(center: Point, radius: f32, inner: f32, content: Option<Div>) -> Div {
    div()
        .absolute()
        .left(px(center.x - radius))
        .top(px(center.y - radius))
        .size(px(2.0 * radius))
        .rounded_full()
        .bg(white())
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .size(px(2.0 * inner))
                .rounded_full()
                .bg(INK)
                .flex()
                .items_center()
                .justify_center()
                .children(content),
        )
}

/// Paints every stroke as the pen outline the bundle draws, at a width that reads on screen.
fn paint(sheet: &Canvas, bounds: Bounds<Pixels>, opacity: f32, window: &mut Window) {
    let area = on_overlay(sheet.part, bounds.size);
    let origin = point(
        bounds.origin.x + area.origin.x,
        bounds.origin.y + area.origin.y,
    );
    let (width, height) = (f32::from(area.size.width), f32::from(area.size.height));
    let pen = ink::pen(width, height);
    let color = Rgba { a: opacity, ..INK };
    for stroke in sheet
        .strokes
        .iter()
        .map(Arc::as_ref)
        .chain(std::iter::once(sheet.drawing.as_slice()))
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
        let mut corners = outline
            .iter()
            .map(|corner| point(origin.x + px(corner.x), origin.y + px(corner.y)));
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
                Ok(Err(error)) => {
                    // No area can be dragged out without the overlay: the Show records the whole
                    // display instead.
                    if let Some(chosen) = &canvas.borrow().picking {
                        chosen.close();
                    }
                    (None, (!reported).then_some(error))
                },
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
    use std::ops::Range;

    use speakeasy_show::{Moment, Rect, Span};

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
        assert!(canvas.update([], stroke(3), None, at(0)));
        assert!(canvas.update([stroke(3).into()], [], None, at(0)));
        assert!(whole(&canvas, at(600)));
        let midway = canvas.opacity(at(975));
        assert!(midway > 0.2 && midway < 0.3, "{midway}");
        assert!(canvas.faded(at(1_300)));
        // Drawing again keeps every mark whole for as long as the stroke takes.
        assert!(canvas.update([], stroke(4), None, at(600)));
        assert!(whole(&canvas, at(5_000)));
    }

    #[test]
    fn held_ink_draws_no_frames_until_it_fades() {
        let at = clock();
        let mut canvas = Canvas::default();
        canvas.update([], stroke(3), None, at(0));
        assert_eq!(canvas.next_frame(at(0)), Redraw::OnChange);
        canvas.update([stroke(3).into()], [], None, at(0));
        let left = Duration::from_millis(50);
        assert_eq!(canvas.next_frame(at(600)), Redraw::After(left));
        assert_eq!(canvas.next_frame(at(975)), Redraw::Animate);
        assert_eq!(canvas.next_frame(at(1_300)), Redraw::OnChange);
    }

    #[test]
    fn marks_drawn_before_the_fade_ends_stay_and_later_ink_starts_clean() {
        let at = clock();
        let mut canvas = Canvas::default();
        canvas.update([], stroke(3), None, at(0));
        canvas.update([stroke(3).into()], [], None, at(100));
        // An arrow drawn while the circle fades keeps it.
        canvas.update([], stroke(4), None, at(1_000));
        canvas.update([stroke(4).into()], [], None, at(1_100));
        assert_eq!(canvas.strokes.len(), 2);
        // A stroke cut for length while the pen keeps drawing stays whole.
        canvas.update([], stroke(5), None, at(1_150));
        assert!(canvas.update([stroke(5).into()], stroke(2), None, at(1_200)));
        assert!(whole(&canvas, at(9_000)));
        assert_eq!(canvas.strokes.len(), 3);
        // A pointer that moved too little to add ink changes nothing.
        assert!(!canvas.update([], stroke(2), None, at(1_300)));
        canvas.update([stroke(2).into()], [], None, at(1_400));
        assert_eq!(canvas.next_frame(at(1_400)), Redraw::After(HOLD));
        // Ink once everything has faded starts a clean canvas.
        assert!(canvas.update([], stroke(3), None, at(2_800)));
        assert_eq!(canvas.strokes, Vec::<Arc<[Point]>>::new());
        assert!(!Canvas::default().update([], [], None, at(0)));
    }

    /// Mark `number`, made of the recording's `strokes`.
    fn mark(number: usize, strokes: Range<usize>) -> Mark {
        Mark {
            number,
            shape: Shape::Tap,
            strokes,
            span: Span::at(Moment::START),
            bounds: Rect {
                left: 0.5,
                top: 0.5,
                right: 0.5,
                bottom: 0.5,
            },
        }
    }

    /// The first `count` marks, numbered from one.
    fn marks(count: usize) -> Arc<[Mark]> {
        (1..=count).map(|number| mark(number, 0..0)).collect()
    }

    /// Marks numbered from one, each made of the recording's strokes from where the mark before
    /// it ended up to one of `ends`.
    fn grouped(ends: &[usize]) -> Arc<[Mark]> {
        let starts = std::iter::once(0).chain(ends.iter().copied());
        (1..)
            .zip(starts.zip(ends))
            .map(|(number, (start, &end))| mark(number, start..end))
            .collect()
    }

    fn shown(canvas: &Canvas) -> Vec<usize> {
        canvas.shown().map(|mark| mark.number).collect()
    }

    #[test]
    fn numbers_show_beside_their_marks_until_the_ink_fades() {
        let at = clock();
        let mut canvas = Canvas::default();
        canvas.update([], stroke(3), None, at(0));
        assert!(canvas.update([stroke(3).into()], [], Some(marks(1)), at(100)));
        assert_eq!(shown(&canvas), [1]);
        // A pin keeps the ink before it and holds it again.
        assert!(canvas.update([], [], Some(marks(2)), at(700)));
        assert_eq!(shown(&canvas), [1, 2]);
        assert_eq!(canvas.next_frame(at(700)), Redraw::After(HOLD));
        // Once everything has faded, a pin starts clean, numbered on from the last mark.
        assert!(canvas.update([], [], Some(marks(3)), at(2_100)));
        assert_eq!(shown(&canvas), [3]);
        assert_eq!(canvas.strokes, Vec::<Arc<[Point]>>::new());
        assert!(whole(&canvas, at(2_100)));
        // So does a stroke.
        canvas.update([], stroke(4), None, at(4_000));
        canvas.update([stroke(4).into()], [], Some(marks(4)), at(4_100));
        assert_eq!(shown(&canvas), [4]);
    }

    #[test]
    fn a_stroke_that_joins_a_faded_mark_shows_its_number() {
        let at = clock();
        let mut canvas = Canvas::default();
        canvas.update([], stroke(3), None, at(0));
        canvas.update([stroke(3).into()], [], Some(grouped(&[1])), at(100));
        // Shift goes down again 1.1 s after the lift, soon enough for the bundle to join the next
        // stroke to mark ①, but the pen pauses and draws only once the circle has faded.
        canvas.update([], stroke(4), None, at(1_450));
        assert_eq!(canvas.strokes, Vec::<Arc<[Point]>>::new());
        canvas.update([stroke(4).into()], [], Some(grouped(&[2])), at(1_700));
        assert_eq!(shown(&canvas), [1]);
        // A stroke after everything has faded again starts a mark of its own.
        canvas.update([], stroke(5), None, at(4_000));
        canvas.update([stroke(5).into()], [], Some(grouped(&[2, 3])), at(4_100));
        assert_eq!(shown(&canvas), [2]);
    }

    #[test]
    fn a_drag_chooses_the_area_it_covers_and_a_stray_click_chooses_nothing() {
        let size = gpui::size(px(1_000.0), px(500.0));
        let at = |x, y| point(px(x), px(y));
        let part = |left, top, right, bottom| Part {
            left,
            top,
            right,
            bottom,
        };
        // Dragged up and to the left.
        assert_eq!(
            dragged(at(300.0, 200.0), at(100.0, 100.0), size),
            Some(part(0.1, 0.2, 0.3, 0.4))
        );
        // Past the display's edge, the area stops at it.
        assert_eq!(
            dragged(at(900.0, 400.0), at(1_200.0, 600.0), size),
            Some(part(0.9, 0.8, 1.0, 1.0))
        );
        assert_eq!(dragged(at(100.0, 100.0), at(110.0, 300.0), size), None);
        assert_eq!(dragged(at(100.0, 100.0), at(100.0, 100.0), size), None);
    }
}
