//! Turning pointer samples into ink, pins, and the clicks and drags that passed through.
//!
//! Holding Shift and moving the pointer draws; clicks keep reaching the apps underneath, so
//! pressing a button never draws and drawing never clicks.

use std::mem;

use crate::{Action, InkPoint, Mark, Moment, Pin, Point, Recording, Size, Span, Stroke, marks};

/// A press that moves further than this many pixels before release is a drag.
const DRAG_PIXELS: f32 = 12.0;
/// Ink samples closer together than this many pixels add nothing to a stroke.
const INK_SPACING_PIXELS: f32 = 1.5;
/// Ink spanning fewer pixels is Shift held to type a capital letter, not drawing.
const SHORTEST_STROKE_PIXELS: f32 = 6.0;
/// A stroke this long is cut, and drawing continues in a new one.
const MOST_STROKE_POINTS: usize = 2_400;

/// Follows the pointer over the captured surface and keeps what it drew, pinned, and did.
#[derive(Debug)]
pub struct Tracker {
    surface: Size,
    /// Where the pointer was last seen.
    pointer: Option<Point>,
    drawing: Vec<InkPoint>,
    /// Whether the stroke being drawn has spanned enough to be ink.
    deliberate: bool,
    press: Option<Press>,
    /// When Shift, down to draw at the last sample, went down.
    shift_down: Option<Moment>,
    /// Each press of Shift that drew ink or was down with the button, by the moment it first did,
    /// earlier first.
    shift_uses: Vec<Moment>,
    strokes: Vec<Stroke>,
    pins: Vec<Pin>,
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
    /// Whether Shift was held to draw at any point in the press, which then records no click or
    /// drag.
    drawing: bool,
}

impl Tracker {
    /// A tracker for a surface `surface` logical pixels in size, which distances are measured in.
    #[must_use]
    pub const fn new(surface: Size) -> Self {
        Self {
            surface,
            pointer: None,
            drawing: Vec::new(),
            deliberate: false,
            press: None,
            shift_down: None,
            shift_uses: Vec::new(),
            strokes: Vec::new(),
            pins: Vec::new(),
            actions: Vec::new(),
        }
    }

    /// Observes the pointer at `at`, in time order, with `drawing` while Shift is held to draw and
    /// `pressed` while the primary button is down. `point` is normalized to the surface, and may
    /// lie outside it when the pointer is on another display.
    pub fn observe(&mut self, at: Moment, point: Point, drawing: bool, pressed: bool) {
        self.pointer = Some(point);
        self.shift_down = drawing.then(|| self.shift_down.unwrap_or(at));
        if drawing && pressed {
            self.use_shift(at);
        }
        let inside = inside(point);
        if drawing && inside {
            self.draw(InkPoint { point, at });
        } else {
            self.lift();
        }
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
                press.reach = press.reach.max(pixels(self.surface, press.from, point));
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

    /// Drops a pin where the pointer was last seen for `taps`, two quick taps of Shift from the
    /// first press to the second release, and returns whether it did. Taps that drew ink or were
    /// down with the button ask for nothing: two quick strokes, as in an X, or two Shift-clicks tap
    /// Shift as a double tap does. A tap that ended a pin begins no other, so three taps pin once.
    /// Nothing drops while the pointer is off the surface or unseen.
    pub fn pin(&mut self, taps: Span) -> bool {
        let used = self
            .shift_uses
            .iter()
            .rev()
            .take_while(|&&used| used >= taps.start())
            .any(|&used| used <= taps.end());
        let pinned = self.pins.last().is_some_and(|pin| taps.start() <= pin.at);
        if !used
            && !pinned
            && let Some(point) = self.pointer
            && inside(point)
        {
            self.pins.push(Pin {
                at: taps.end(),
                point,
            });
            true
        } else {
            false
        }
    }

    /// The marks the finished strokes and pins make so far, numbered as the bundle will number
    /// them: a later stroke or pin never renumbers an earlier mark.
    #[must_use]
    pub fn marks(&self) -> Vec<Mark> {
        marks::group(&self.strokes, &self.pins, self.surface)
    }

    /// Ends any stroke or press in progress at `at` and returns what was recorded, lasting until
    /// `at` and not yet narrated.
    #[must_use]
    pub fn finish(mut self, at: Moment) -> Recording {
        self.lift();
        if let Some(press) = self.press {
            self.release(at, press.to);
        }
        Recording {
            duration: at,
            surface: self.surface,
            strokes: self.strokes,
            pins: self.pins,
            actions: self.actions,
            ..Recording::default()
        }
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
            self.cut();
        }
        self.deliberate |= self.drawing.first().is_some_and(|first| {
            pixels(self.surface, first.point, ink.point) >= SHORTEST_STROKE_PIXELS
        });
        if self.deliberate {
            self.use_shift(ink.at);
        }
        self.drawing.push(ink);
    }

    /// Notes that the press of Shift down now drew ink or was down with the button at `at`; whether
    /// a press was used is all a pin asks, so only its first use is kept.
    fn use_shift(&mut self, at: Moment) {
        let noted = self
            .shift_down
            .is_some_and(|down| self.shift_uses.last().is_some_and(|&used| used >= down));
        if !noted {
            self.shift_uses.push(at);
        }
    }

    /// Ends the stroke being drawn without lifting the pen: the next stroke starts where this one
    /// ended, as deliberate as it was.
    fn cut(&mut self) {
        let (joint, deliberate) = (self.drawing.last().copied(), self.deliberate);
        self.lift();
        self.drawing.extend(joint);
        self.deliberate = deliberate;
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

/// Whether a normalized point lies on the surface.
fn inside(point: Point) -> bool {
    (0.0..=1.0).contains(&point.x) && (0.0..=1.0).contains(&point.y)
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

    /// Replays `(milliseconds, x, y, drawing, pressed)` samples, finishing after the last, and
    /// returns the strokes and actions recorded.
    fn replay(samples: &[(u64, f32, f32, bool, bool)]) -> (Vec<Stroke>, Vec<Action>) {
        let mut tracker = Tracker::new(SURFACE);
        let mut end = Moment::START;
        for &(millis, x, y, drawing, pressed) in samples {
            end = Moment::from_millis(millis);
            tracker.observe(end, Point::new(x, y), drawing, pressed);
        }
        let recording = tracker.finish(end);
        (recording.strokes, recording.actions)
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
        let strokes = tracker.finish(Moment::from_millis(2_402)).strokes;
        assert_eq!(strokes.len(), 2);
        assert_eq!(strokes[0].points().last(), strokes[1].points().first());
    }

    /// How a test taps Shift.
    #[derive(Clone, Copy)]
    enum Tap {
        Clean,
        /// Flicking the pointer 20 pixels right, which draws a quick stroke.
        Flick,
        /// Holding the button down.
        ShiftClick,
    }

    /// Taps Shift for 60 ms from `millis` with the pointer at `x`, 0.4, as `kind` says; returns
    /// the tap's span.
    fn tap(tracker: &mut Tracker, millis: u64, x: f32, kind: Tap) -> Span {
        let at = |offset: u64| Moment::from_millis(millis.saturating_add(offset));
        let (flicked, pressed) = match kind {
            Tap::Clean => (x, false),
            Tap::Flick => (x + 0.02, false),
            Tap::ShiftClick => (x, true),
        };
        tracker.observe(at(0), Point::new(x, 0.4), true, pressed);
        tracker.observe(at(30), Point::new(flicked, 0.4), true, pressed);
        tracker.observe(at(60), Point::new(flicked, 0.4), false, false);
        Span::new(at(0), at(60))
    }

    fn clean(tracker: &mut Tracker, millis: u64, x: f32) -> Span {
        tap(tracker, millis, x, Tap::Clean)
    }

    /// Asks for the pin of a double tap from `first`'s press to `second`'s release.
    fn pin(tracker: &mut Tracker, first: Span, second: Span) -> bool {
        tracker.pin(Span::new(first.start(), second.end()))
    }

    #[test]
    fn pins_drop_where_the_pointer_was_last_seen_on_the_surface_after_two_clean_taps() {
        let mut tracker = Tracker::new(SURFACE);
        let t = &mut tracker;
        assert!(
            !t.pin(Span::at(Moment::START)),
            "the pointer has not been seen"
        );
        let (first, second) = (clean(t, 0, 0.3), clean(t, 160, 0.3));
        assert!(pin(t, first, second));
        let third = clean(t, 320, 0.3);
        assert!(!pin(t, second, third), "three taps pin once");
        let (first, second) = (clean(t, 1_000, 1.3), clean(t, 1_160, 1.3));
        assert!(!pin(t, first, second), "on another display");
        let (first, second) = (tap(t, 2_000, 0.5, Tap::ShiftClick), clean(t, 2_160, 0.5));
        assert!(!pin(t, first, second), "Shift-clicking");
        let (first, second) = (
            tap(t, 3_000, 0.5, Tap::Flick),
            tap(t, 3_160, 0.5, Tap::Flick),
        );
        assert!(!pin(t, first, second), "two quick strokes, as in an X");
        let stroke = tap(t, 4_000, 0.6, Tap::Flick);
        let (first, second) = (clean(t, 4_160, 0.62), clean(t, 4_320, 0.62));
        assert!(!pin(t, stroke, first), "a quick stroke and a tap");
        assert!(
            pin(t, first, second),
            "a quick stroke costs the double tap after it nothing"
        );
        let (first, second) = (clean(t, 5_000, 0.7), clean(t, 5_160, 0.7));
        tap(t, 5_240, 0.7, Tap::Flick);
        assert!(
            pin(t, first, second),
            "a stroke seen after the taps but before their pin"
        );
        let recording = tracker.finish(Moment::from_millis(6_000));
        assert_eq!(
            recording.pins,
            [
                Pin {
                    at: Moment::from_millis(220),
                    point: Point::new(0.3, 0.4)
                },
                Pin {
                    at: Moment::from_millis(4_380),
                    point: Point::new(0.62, 0.4)
                },
                Pin {
                    at: Moment::from_millis(5_220),
                    point: Point::new(0.7 + 0.02, 0.4)
                }
            ]
        );
        assert_eq!(recording.strokes.len(), 4, "the flicks drew");
        assert_eq!(recording.duration, Moment::from_millis(6_000));
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
