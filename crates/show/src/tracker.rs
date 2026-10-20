//! Turning pointer samples into ink and into the clicks and drags that passed through.
//!
//! Holding Shift alone and moving the pointer draws; clicks keep reaching the apps underneath, so
//! pressing a button never draws and drawing never clicks.

use crate::{
    Moment, Point, Span,
    recording::{Action, InkPoint, Size, Stroke},
};

/// A press that moves further than this many pixels before release is a drag.
const DRAG_PIXELS: f32 = 12.0;
/// Ink samples closer together than this many pixels add nothing to a stroke.
const INK_SPACING_PIXELS: f32 = 1.5;
/// Ink spanning fewer pixels is Shift held to type a capital letter, not drawing.
const SHORTEST_STROKE_PIXELS: f32 = 6.0;
/// A stroke this long is cut, and drawing continues in a new one.
const MOST_STROKE_POINTS: usize = 2_400;

/// Follows the pointer over the captured surface and keeps what it drew and did.
#[derive(Debug)]
pub struct Tracker {
    surface: Size,
    drawing: Vec<InkPoint>,
    press: Option<Press>,
    strokes: Vec<Stroke>,
    actions: Vec<Action>,
}

/// A button press in progress.
#[derive(Clone, Copy, Debug)]
struct Press {
    at: Moment,
    from: Point,
    /// The farthest the pointer has moved from `from`, in pixels.
    reach: f32,
    /// Pressed while drawing, so it belongs to no click.
    drawing: bool,
}

impl Tracker {
    /// A tracker for a surface `surface` logical pixels in size, which distances are measured in.
    #[must_use]
    pub const fn new(surface: Size) -> Self {
        Self {
            surface,
            drawing: Vec::new(),
            press: None,
            strokes: Vec::new(),
            actions: Vec::new(),
        }
    }

    /// Observes the pointer at `at`, in time order. `point` is normalized to the surface, and may
    /// lie outside it when the pointer is on another display.
    pub fn observe(&mut self, at: Moment, point: Point, drawing: bool, pressed: bool) {
        let inside = (0.0..=1.0).contains(&point.x) && (0.0..=1.0).contains(&point.y);
        if drawing && inside {
            self.draw(InkPoint { point, at });
        } else {
            self.lift();
        }
        let surface = self.surface;
        match (self.press.as_mut(), pressed) {
            (None, true) if inside => {
                self.press = Some(Press {
                    at,
                    from: point,
                    reach: 0.0,
                    drawing,
                });
            },
            (Some(press), true) => {
                press.reach = press.reach.max(pixels(surface, press.from, point));
                press.drawing |= drawing;
            },
            (Some(_), false) => self.release(at, point),
            (None, _) => {},
        }
    }

    /// The stroke being drawn, if any.
    #[must_use]
    pub fn drawing(&self) -> &[InkPoint] {
        &self.drawing
    }

    /// The strokes finished so far, in drawing order.
    #[must_use]
    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

    /// Ends any stroke or press in progress at `at` and returns the ink and actions recorded.
    #[must_use]
    pub fn finish(mut self, at: Moment) -> (Vec<Stroke>, Vec<Action>) {
        self.lift();
        if let Some(press) = self.press.take()
            && !press.drawing
        {
            self.actions.push(Action::Click {
                at,
                point: press.from,
            });
        }
        (self.strokes, self.actions)
    }

    fn draw(&mut self, ink: InkPoint) {
        if self.drawing.len() >= MOST_STROKE_POINTS {
            self.lift();
        }
        let spaced = self
            .drawing
            .last()
            .is_none_or(|last| self.pixels(last.point, ink.point) >= INK_SPACING_PIXELS);
        if spaced {
            self.drawing.push(ink);
        }
    }

    /// Ends the stroke being drawn, keeping it only if it spans enough to be deliberate.
    fn lift(&mut self) {
        let points = std::mem::take(&mut self.drawing);
        let Some(stroke) = Stroke::new(points) else {
            return;
        };
        let (first, last) = (stroke.first().point, stroke.last().point);
        let span = stroke
            .points()
            .iter()
            .map(|ink| self.pixels(first, ink.point))
            .fold(self.pixels(first, last), f32::max);
        if span >= SHORTEST_STROKE_PIXELS {
            self.strokes.push(stroke);
        }
    }

    fn release(&mut self, at: Moment, point: Point) {
        let Some(press) = self.press.take() else {
            return;
        };
        if press.drawing {
            return;
        }
        let reach = press.reach.max(self.pixels(press.from, point));
        self.actions.push(if reach < DRAG_PIXELS {
            Action::Click {
                at,
                point: press.from,
            }
        } else {
            Action::Drag {
                span: Span::new(press.at, at),
                from: press.from,
                to: point,
            }
        });
    }

    fn pixels(&self, one: Point, other: Point) -> f32 {
        pixels(self.surface, one, other)
    }
}

/// The distance between two normalized points, in pixels of `surface`.
fn pixels(surface: Size, one: Point, other: Point) -> f32 {
    (one - other).scaled(surface.width, surface.height).length()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SURFACE: Size = Size {
        width: 1000.0,
        height: 500.0,
    };

    /// Replays `(milliseconds, x, y, drawing, pressed)` samples, finishing after the last.
    fn replay(samples: &[(u64, f32, f32, bool, bool)]) -> (Vec<Stroke>, Vec<Action>) {
        let mut tracker = Tracker::new(SURFACE);
        let mut end = Moment::START;
        for &(millis, x, y, drawing, pressed) in samples {
            end = Moment::from_millis(millis);
            tracker.observe(end, Point::new(x, y), drawing, pressed);
        }
        tracker.finish(end)
    }

    #[test]
    fn shift_and_movement_draw_a_stroke() {
        let (strokes, actions) = replay(&[
            (0, 0.10, 0.10, true, false),
            (8, 0.12, 0.10, true, false),
            (16, 0.14, 0.11, true, false),
            (24, 0.14, 0.11, false, false),
        ]);
        assert_eq!(strokes.len(), 1);
        assert_eq!(strokes[0].points().len(), 3);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn shift_for_a_capital_letter_draws_nothing() {
        let (strokes, _) = replay(&[
            (0, 0.500, 0.5, true, false),
            (8, 0.502, 0.5, true, false),
            (16, 0.502, 0.5, false, false),
        ]);
        assert_eq!(strokes, Vec::<Stroke>::new());
    }

    #[test]
    fn presses_become_clicks_or_drags_by_how_far_they_moved() {
        let (_, actions) = replay(&[
            (0, 0.50, 0.50, false, true),
            (8, 0.505, 0.50, false, true),
            (100, 0.505, 0.50, false, false),
            (200, 0.20, 0.20, false, true),
            (300, 0.40, 0.20, false, true),
            (400, 0.40, 0.20, false, false),
        ]);
        assert_eq!(
            actions,
            [
                Action::Click {
                    at: Moment::from_millis(100),
                    point: Point::new(0.50, 0.50)
                },
                Action::Drag {
                    span: Span::new(Moment::from_millis(200), Moment::from_millis(400)),
                    from: Point::new(0.20, 0.20),
                    to: Point::new(0.40, 0.20)
                }
            ]
        );
    }

    #[test]
    fn drawing_never_clicks_and_other_displays_are_ignored() {
        let (strokes, actions) = replay(&[
            (0, 0.30, 0.30, true, true),
            (8, 0.35, 0.30, true, true),
            (16, 0.35, 0.30, false, false),
            (100, 1.40, 0.30, false, true),
            (108, 1.40, 0.30, false, false),
            (200, 1.20, 0.50, true, false),
            (208, 1.30, 0.50, true, false),
        ]);
        assert_eq!(strokes.len(), 1);
        assert_eq!(actions, Vec::<Action>::new());
    }

    #[test]
    fn a_press_held_at_the_end_still_counts() {
        let (_, actions) = replay(&[(0, 0.5, 0.5, false, true), (50, 0.5, 0.5, false, true)]);
        assert_eq!(actions.len(), 1);
    }
}
