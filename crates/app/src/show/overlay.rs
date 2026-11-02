use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow};
use async_channel::Sender;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Div, FillOptions, FillRule, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, PathStyle, Pixels, Render, Rgba,
    Size, Task, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, canvas, div, point, prelude::*, px, relative, white,
};
use speakeasy_platform::{Display, NativeWindow, Part};
use speakeasy_show::{Ending, Mark, Point, Shape, ink};
use tokio::sync::watch;

use crate::{
    BRAND_FONT,
    gpui_ext::{EntityUpdate, WindowUpdate, defer_native, follow},
    runtime::{Phase, Snapshot},
};

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

const DIGIT_EM: f32 = 0.6;

const FRAME: Pixels = px(2.0);

const SHADE: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.4,
};

const SMALLEST_AREA: f32 = 24.0;

#[derive(Clone, Debug, Default)]
pub(crate) enum Canvas {
    #[default]
    Hidden,
    Picking {
        display: Display,
        chosen: Sender<Part>,
    },
    Covering {
        display: Display,
        sheet: Sheet,
    },
}

impl Canvas {
    pub(crate) fn covering(display: Display, part: Part) -> Self {
        Self::Covering {
            display,
            sheet: Sheet::new(part),
        }
    }

    pub(crate) const fn display(&self) -> Option<Display> {
        match self {
            Self::Hidden => None,
            Self::Picking { display, .. } | Self::Covering { display, .. } => Some(*display),
        }
    }

    pub(crate) fn sheet_mut(&mut self) -> Option<&mut Sheet> {
        match self {
            Self::Covering { sheet, .. } => Some(sheet),
            Self::Hidden | Self::Picking { .. } => None,
        }
    }

    fn cover(&self) -> Option<Cover> {
        match self {
            Self::Hidden => None,
            Self::Picking { display, .. } => Some(Cover {
                display: *display,
                picking: true,
            }),
            Self::Covering { display, .. } => Some(Cover {
                display: *display,
                picking: false,
            }),
        }
    }

    fn chosen(&self) -> Option<&Sender<Part>> {
        match self {
            Self::Picking { chosen, .. } => Some(chosen),
            Self::Hidden | Self::Covering { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Cover {
    display: Display,
    picking: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Sheet {
    part: Part,
    strokes: Vec<Arc<[Point]>>,
    cleared: usize,
    drawing: Vec<Point>,
    marks: Arc<[Mark]>,
    first: usize,
    lifted: Option<Instant>,
}

impl Sheet {
    fn new(part: Part) -> Self {
        Self {
            part,
            strokes: Vec::new(),
            cleared: 0,
            drawing: Vec::new(),
            marks: Arc::new([]),
            first: 1,
            lifted: None,
        }
    }

    pub(crate) fn track(&mut self, part: Part) -> bool {
        let moved = part != self.part;
        self.part = part;
        moved
    }

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

    fn shown(&self) -> impl Iterator<Item = &Mark> {
        self.marks.iter().filter(|mark| mark.number >= self.first)
    }

    fn since_lift(&self, now: Instant) -> Option<Duration> {
        self.lifted
            .filter(|_| self.drawing.is_empty())
            .map(|lifted| now.saturating_duration_since(lifted))
    }

    fn opacity(&self, now: Instant) -> f32 {
        self.since_lift(now).map_or(1.0, |since| {
            let fading = since.saturating_sub(HOLD).div_duration_f32(FADE).min(1.0);
            (1.0 - fading).powi(2)
        })
    }

    fn faded(&self, now: Instant) -> bool {
        self.opacity(now) <= 0.0
    }

    fn next_frame(&self, now: Instant) -> Redraw {
        match self.since_lift(now) {
            Some(since) if since < HOLD => Redraw::After(HOLD.saturating_sub(since)),
            Some(_) if self.opacity(now) > 0.0 => Redraw::Animate,
            _ => Redraw::OnChange,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Redraw {
    OnChange,
    After(Duration),
    Animate,
}

struct Overlay {
    canvas: watch::Receiver<Canvas>,
    window: AnyWindowHandle,
    cover: Option<Cover>,
    misplaced: Sender<anyhow::Error>,
    drag: Option<[gpui::Point<Pixels>; 2]>,
    placement: Option<Task<()>>,
    wake: Option<Task<()>>,
    _updates: Task<()>,
}

impl Overlay {
    fn new(
        canvas: watch::Receiver<Canvas>,
        misplaced: Sender<anyhow::Error>,
        window: &Window,
        cx: &Context<'_, Self>,
    ) -> Self {
        Self {
            _updates: follow(canvas.clone(), cx, Canvas::cover, Self::cover),
            canvas,
            window: window.window_handle(),
            cover: None,
            misplaced,
            drag: None,
            placement: None,
            wake: None,
        }
    }

    fn cover(&mut self, cover: Option<Cover>, cx: &mut Context<'_, Self>) {
        if cover != self.cover {
            self.cover = cover;
            if !cover.is_some_and(|cover| cover.picking) {
                self.drag = None;
            }
            let chosen = self.canvas.borrow().chosen().cloned();
            let misplaced = self.misplaced.clone();
            self.placement = Some(defer_native(
                self.window,
                "overlay",
                cx,
                move |native, _| {
                    let Err(error) = place(native, cover) else {
                        return;
                    };
                    if let Some(chosen) = chosen {
                        chosen.close();
                    }
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "A full lane already holds the Show's first failure, and a closed one belongs to a Show that ended"
                    )]
                    let _ = misplaced.try_send(error);
                },
            ));
        }
        cx.notify();
    }

    fn pick(&mut self, to: gpui::Point<Pixels>, size: Size<Pixels>, cx: &mut Context<'_, Self>) {
        let Some([from, _]) = self.drag else {
            return;
        };
        let sent = self
            .canvas
            .borrow()
            .chosen()
            .is_some_and(|chosen| answer(chosen, from, to, size));
        self.drag = sent.then_some([from, to]);
        cx.notify();
    }

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
        let release = || {
            cx.listener(|this: &mut Self, event: &MouseUpEvent, window, cx| {
                this.pick(event.position, window.viewport_size(), cx);
            })
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
            .on_mouse_up(MouseButton::Left, release())
            .on_mouse_up_out(MouseButton::Left, release())
            .children(shades)
            .children(hole.map(frame))
            .when(hole.is_none(), |picker| picker.child(hint()))
    }
}

fn place(native: &NativeWindow, cover: Option<Cover>) -> anyhow::Result<()> {
    let placed = cover.map(|cover| native.configure_overlay(cover.display));
    let covered = matches!(placed, Some(Ok(())));
    native.set_overlay_input(covered && cover.is_some_and(|cover| cover.picking));
    native.set_overlay_visible(covered);
    placed.unwrap_or(Ok(()))
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let current = self.canvas.borrow().clone();
        let size = window.viewport_size();
        let sheet = match current {
            Canvas::Covering { sheet, .. } => sheet,
            Canvas::Picking { .. } => {
                self.wake = None;
                return self.picker(size, cx);
            },
            Canvas::Hidden => {
                self.wake = None;
                return div();
            },
        };
        let now = Instant::now();
        let opacity = sheet.opacity(now);
        self.wake = match sheet.next_frame(now) {
            Redraw::OnChange => None,
            Redraw::After(delay) => Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
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

fn on_overlay(part: Part, size: Size<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(size.width * part.left, size.height * part.top),
        gpui::size(size.width * part.width(), size.height * part.height()),
    )
}

fn answer(
    chosen: &Sender<Part>,
    from: gpui::Point<Pixels>,
    to: gpui::Point<Pixels>,
    size: Size<Pixels>,
) -> bool {
    if let Some(part) = dragged(from, to, size) {
        chosen.try_send(part).is_ok()
    } else {
        chosen.close();
        false
    }
}

fn dragged(from: gpui::Point<Pixels>, to: gpui::Point<Pixels>, size: Size<Pixels>) -> Option<Part> {
    let normalized = |point: gpui::Point<Pixels>| [point.x / size.width, point.y / size.height];
    let part = Part::between(normalized(from), normalized(to));
    let area = on_overlay(part, size).size;
    (f32::from(area.width) >= SMALLEST_AREA && f32::from(area.height) >= SMALLEST_AREA)
        .then_some(part)
}

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
                .child("Drag over the area to record · Click to cancel"),
        )
}

fn label(mark: &Mark, area: Bounds<Pixels>) -> Div {
    let (width, height) = (f32::from(area.size.width), f32::from(area.size.height));
    let radius = ink::label_radius(width, height);
    let center = mark.label_center(width, height, radius);
    let digits = mark.number.to_string();
    let size = (ink::LABEL_TYPE * radius)
        .min(ink::LABEL_DIGITS * radius / (DIGIT_EM * digits.len() as f32));
    let number = disc(
        center,
        radius,
        radius * (1.0 - ink::LABEL_RING),
        Some(
            div()
                .font_family(BRAND_FONT)
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

fn paint(sheet: &Sheet, bounds: Bounds<Pixels>, opacity: f32, window: &mut Window) {
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
                .wait_for(|canvas| canvas.display().is_some())
                .await
                .is_err()
            {
                return;
            }
            let show = snapshots.borrow().session;
            let (misplaced, misplacements) = async_channel::bounded(1);
            let opened = cx.update(|cx| open(canvas.clone(), misplaced, cx));
            if opened.is_err()
                && let Some(chosen) = canvas.borrow().chosen()
            {
                chosen.close();
            }
            let released = canvas
                .wait_for(|canvas| canvas.display().is_none())
                .await
                .is_ok();
            let failure = match opened {
                Ok(overlay) => {
                    overlay.update_if_open(cx, |_, window, _| window.remove_window());
                    misplacements.try_recv().ok()
                },
                Err(error) => Some(error),
            };
            if !released {
                return;
            }
            if reported {
                continue;
            }
            let Some(error) = failure else {
                continue;
            };

            let Ok(failure) = snapshots
                .wait_for(|snapshot| snapshot.session != show || !snapshot.phase.is_active())
                .await
                .map(|snapshot| {
                    if snapshot.session == show {
                        failure_after(&snapshot, &error)
                    } else {
                        None
                    }
                })
            else {
                return;
            };
            let Some(error) = failure else {
                continue;
            };
            reported = true;
            cx.update(|cx| report(&error, cx));
        }
    })
    .detach();
}

fn failure_after(snapshot: &Snapshot, error: &anyhow::Error) -> Option<anyhow::Error> {
    match snapshot.phase {
        Phase::Copied
            if snapshot.ending == Ending::Finished && snapshot.narration_lost.is_none() =>
        {
            Some(anyhow!(
                "Show copied, but its ink could not appear on screen: {error:#}"
            ))
        },
        Phase::Copied => Some(anyhow!(
            "{} Its ink could not appear on screen: {error:#}",
            snapshot.message
        )),
        Phase::Unchosen => Some(anyhow!(
            "The Show was cancelled because its area shade could not appear: {error:#}"
        )),
        _ => None,
    }
}

fn open(
    canvas: watch::Receiver<Canvas>,
    misplaced: Sender<anyhow::Error>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Overlay>> {
    let display = cx.primary_display().context("No display available")?;
    cx.open_window(
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
            cx.new(|cx| Overlay::new(canvas, misplaced, window, cx))
        },
    )
}

#[cfg(test)]
mod tests;
