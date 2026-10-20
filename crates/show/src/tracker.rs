//! Turning pointer samples into ink and into the clicks and drags that passed through.
//!
//! Holding Shift alone and moving the pointer draws; clicks keep reaching the apps underneath, so
//! pressing a button never draws and drawing never clicks.

use std::mem;

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
    /// Whether the stroke being drawn has spanned enough to be ink.
    deliberate: bool,
    press: Option<Press>,
    strokes: Vec<Stroke>,
    actions: Vec<Action>,
}

/// A button press in progress.
#[derive(Clone, Copy, Debug)]
struct Press {
    at: Moment,
    from: Point,
    /// Where the pointer is now.
    to: Point,
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
            deliberate: false,
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
                    to: point,
                    reach: 0.0,
                    drawing,
                });
            },
            (Some(press), true) => {
                press.to = point;
                press.reach = press.reach.max(pixels(surface, press.from, point));
                press.drawing |= drawing;
            },
            (Some(_), false) => self.release(at, point),
            (None, _) => {},
        }
    }

    /// The stroke being drawn, once it spans enough to be ink rather than Shift held for a capital
    /// letter; until then, nothing.
    #[must_use]
    pub fn drawing(&self) -> &[InkPoint] {
        if self.deliberate { &self.drawing } else { &[] }
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
        if let Some(press) = self.press {
            self.release(at, press.to);
        }
        (self.strokes, self.actions)
    }

    fn draw(&mut self, ink: InkPoint) {
        let spaced = self
            .drawing
            .last()
            .is_none_or(|last| pixels(self.surface, last.point, ink.point) >= INK_SPACING_PIXELS);
        if !spaced {
            return;
        }
        if self.drawing.len() >= MOST_STROKE_POINTS {
            // A cut is no pen lift: the next stroke starts where this one ended, as deliberate.
            let (joint, deliberate) = (self.drawing.last().copied(), self.deliberate);
            self.lift();
            self.drawing.extend(joint);
            self.deliberate = deliberate;
        }
        self.deliberate |= self.drawing.first().is_some_and(|first| {
            pixels(self.surface, first.point, ink.point) >= SHORTEST_STROKE_PIXELS
        });
        self.drawing.push(ink);
    }

    /// Ends the stroke being drawn, keeping it only if it spans enough to be deliberate.
    fn lift(&mut self) {
        let points = mem::take(&mut self.drawing);
        if mem::take(&mut self.deliberate)
            && let Some(stroke) = Stroke::new(points)
        {
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
        let reach = press.reach.max(pixels(self.surface, press.from, point));
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
    fn ink_shows_once_it_spans_enough_to_be_deliberate() {
        let mut tracker = Tracker::new(SURFACE);
        tracker.observe(Moment::START, Point::new(0.500, 0.5), true, false);
        tracker.observe(Moment::from_millis(8), Point::new(0.502, 0.5), true, false);
        assert!(tracker.drawing().is_empty(), "Shift alone shows no ink");
        tracker.observe(Moment::from_millis(16), Point::new(0.510, 0.5), true, false);
        assert_eq!(tracker.drawing().len(), 3);
    }

    #[test]
    fn a_stroke_cut_for_length_keeps_drawing_one_joined_line() {
        let mut tracker = Tracker::new(SURFACE);
        // Laps of a circle 200 pixels in radius, sampled every 2 pixels.
        let draw = |tracker: &mut Tracker, index: u16| {
            let turn = f32::from(index) / 100.0;
            let point = Point::new(0.5 + 0.2 * turn.cos(), 0.5 + 0.4 * turn.sin());
            tracker.observe(Moment::from_millis(index.into()), point, true, false);
        };
        let cut = u16::try_from(MOST_STROKE_POINTS).unwrap();
        for index in 0..=cut {
            draw(&mut tracker, index);
        }
        let ended = tracker.strokes()[0].points().last().copied();
        assert_eq!(tracker.drawing().first().copied(), ended);
        draw(&mut tracker, cut + 1);
        let (strokes, _) = tracker.finish(Moment::from_millis(2_402));
        assert_eq!(strokes.len(), 2);
        assert_eq!(strokes[0].points().last(), strokes[1].points().first());
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
    fn a_press_held_at_the_end_still_counts_as_what_it_was() {
        let (_, actions) = replay(&[(0, 0.5, 0.5, false, true), (50, 0.5, 0.5, false, true)]);
        assert_eq!(
            actions,
            [Action::Click {
                at: Moment::from_millis(50),
                point: Point::new(0.5, 0.5)
            }]
        );
        let (_, actions) = replay(&[(0, 0.2, 0.2, false, true), (50, 0.4, 0.2, false, true)]);
        assert_eq!(
            actions,
            [Action::Drag {
                span: Span::new(Moment::START, Moment::from_millis(50)),
                from: Point::new(0.2, 0.2),
                to: Point::new(0.4, 0.2)
            }]
        );
    }
}
