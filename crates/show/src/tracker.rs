use std::mem;

use crate::{
    Action, InkPoint, Mark, Moment, Pin, Point, Recording, Rect, Size, Spaces, Span, Stroke, marks,
    recording::spaces::{self, PinSpace, Space},
};

const DRAG_PIXELS: f32 = 12.0;

const INK_SPACING_PIXELS: f32 = 1.5;

const SHORTEST_STROKE_PIXELS: f32 = 6.0;

const MOST_STROKE_POINTS: usize = 2_400;

#[derive(Debug)]
pub struct Tracker {
    surface: Size,
    space: Option<Space>,
    generation: Option<u64>,
    drawn_space: Option<Space>,
    last_drawn_space: Option<Space>,
    spaces: Spaces,
    pointer: Option<InkPoint>,
    drawing: Vec<InkPoint>,
    deliberate: bool,
    press: Button,
    pin_after: Option<Moment>,
    shift_down: Option<Moment>,
    shift_uses: Vec<Moment>,
    strokes: Vec<Stroke>,
    pins: Vec<Pin>,
    actions: Vec<Action>,
}

#[derive(Clone, Copy, Debug)]
struct Press {
    at: Moment,
    from: Point,
    to: Point,
    space: Option<Space>,
    reach: f32,
    drawing: bool,
}

#[derive(Clone, Copy, Debug)]
enum Button {
    Up,
    Down(Press),
    Unknown,
}

impl Tracker {
    #[must_use]
    pub const fn new(surface: Size) -> Self {
        Self {
            surface,
            space: None,
            generation: Some(0),
            drawn_space: None,
            last_drawn_space: None,
            spaces: Spaces {
                strokes: Vec::new(),
                pins: Vec::new(),
                actions: Vec::new(),
            },
            pointer: None,
            drawing: Vec::new(),
            deliberate: false,
            press: Button::Up,
            pin_after: None,
            shift_down: None,
            shift_uses: Vec::new(),
            strokes: Vec::new(),
            pins: Vec::new(),
            actions: Vec::new(),
        }
    }

    pub const fn resize(&mut self, surface: Size) {
        self.surface = surface;
    }

    pub(crate) fn locate(&mut self, part: Rect) -> bool {
        if !spaces::valid(part) {
            return false;
        }
        if self.space.is_none_or(|space| space.part != part) {
            self.generation = self
                .generation
                .and_then(|generation| generation.checked_add(1));
            self.space = self.generation.map(|generation| Space { generation, part });
        }
        true
    }

    pub fn gap(&mut self, at: Moment) {
        self.lift();
        self.space = None;
        self.pointer = None;
        self.press = Button::Unknown;
        self.shift_down = None;
        self.pin_after = Some(self.pin_after.unwrap_or(at).max(at));
    }

    pub fn observe(&mut self, at: Moment, point: Point, drawing: bool, pressed: bool) {
        if self.pointer.is_none()
            && let Some(gap) = &mut self.pin_after
        {
            *gap = (*gap).max(at);
        }
        self.pointer = Some(InkPoint { point, at });
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
        match (&mut self.press, pressed) {
            (Button::Up, true) if inside => {
                self.press = Button::Down(Press {
                    at,
                    from: point,
                    to: point,
                    space: self.space,
                    reach: 0.0,
                    drawing,
                });
            },
            (Button::Down(press), true) => {
                press.to = point;
                if press.space != self.space {
                    press.space = None;
                }
                press.reach = press.reach.max(pixels(self.surface, press.from, point));
                press.drawing |= drawing;
            },
            (Button::Down(_), false) => self.release(at, point, self.space),
            (Button::Unknown, false) => self.press = Button::Up,
            (Button::Up | Button::Unknown, _) => {},
        }
    }

    #[must_use]
    pub fn drawing(&self) -> &[InkPoint] {
        if self.deliberate { &self.drawing } else { &[] }
    }

    #[must_use]
    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

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
            && self.pin_after.is_none_or(|gap| taps.start() > gap)
            && let Some(InkPoint {
                point,
                at: observed,
            }) = self.pointer
            && inside(point)
        {
            self.pins.push(Pin {
                at: taps.end(),
                point,
            });
            self.spaces
                .pins
                .push(self.space.map(|space| PinSpace { space, observed }));
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn marks(&self) -> Vec<Mark> {
        marks::group(&self.strokes, &self.pins, self.surface)
    }

    #[must_use]
    pub fn finish(mut self, at: Moment) -> Recording {
        self.lift();
        if let Button::Down(press) = self.press {
            self.release(at, press.to, press.space);
        }
        Recording {
            duration: at,
            surface: self.surface,
            strokes: self.strokes,
            pins: self.pins,
            actions: self.actions,
            spaces: self.spaces,
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
        if self.drawing.is_empty() {
            self.drawn_space = self.space;
        } else if self.drawn_space != self.space {
            self.drawn_space = None;
        }
        self.last_drawn_space = self.space;
        self.deliberate |= self.drawing.first().is_some_and(|first| {
            pixels(self.surface, first.point, ink.point) >= SHORTEST_STROKE_PIXELS
        });
        if self.deliberate {
            self.use_shift(ink.at);
        }
        self.drawing.push(ink);
    }

    fn use_shift(&mut self, at: Moment) {
        let noted = self
            .shift_down
            .is_some_and(|down| self.shift_uses.last().is_some_and(|&used| used >= down));
        if !noted {
            self.shift_uses.push(at);
        }
    }

    fn cut(&mut self) {
        let (joint, deliberate) = (self.drawing.last().copied(), self.deliberate);
        let space = self.last_drawn_space;
        self.lift();
        self.drawing.extend(joint);
        self.drawn_space = space;
        self.last_drawn_space = space;
        self.deliberate = deliberate;
    }

    fn lift(&mut self) {
        let points = mem::take(&mut self.drawing);
        let space = self.drawn_space.take();
        self.last_drawn_space = None;
        if mem::take(&mut self.deliberate)
            && let Some(stroke) = Stroke::new(points)
        {
            self.strokes.push(stroke);
            self.spaces.strokes.push(space);
        }
    }

    fn release(&mut self, at: Moment, point: Point, space: Option<Space>) {
        let Button::Down(press) = mem::replace(&mut self.press, Button::Up) else {
            return;
        };
        if press.drawing {
            return;
        }
        self.spaces
            .actions
            .push(press.space.filter(|proof| Some(*proof) == space));
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

fn inside(point: Point) -> bool {
    (0.0..=1.0).contains(&point.x) && (0.0..=1.0).contains(&point.y)
}

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

    #[derive(Clone, Copy)]
    enum Tap {
        Clean,
        Flick,
        ShiftClick,
    }

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

    fn pin(tracker: &mut Tracker, first: Span, second: Span) -> bool {
        tracker.pin(Span::new(first.start(), second.end()))
    }

    #[test]
    fn pins_drop_where_the_pointer_was_last_seen_on_the_surface_after_two_clean_taps() {
        let mut tracker = Tracker::new(SURFACE);
        let t = &mut tracker;
        assert!(
            !t.pin(Span::at(Moment::START)),
            "The pointer has not been seen"
        );
        let (first, second) = (clean(t, 0, 0.3), clean(t, 160, 0.3));
        assert!(pin(t, first, second));
        let third = clean(t, 320, 0.3);
        assert!(!pin(t, second, third), "Three taps pin once");
        let (first, second) = (clean(t, 1_000, 1.3), clean(t, 1_160, 1.3));
        assert!(!pin(t, first, second), "On another display");
        let (first, second) = (tap(t, 2_000, 0.5, Tap::ShiftClick), clean(t, 2_160, 0.5));
        assert!(!pin(t, first, second), "Shift-clicking");
        let (first, second) = (
            tap(t, 3_000, 0.5, Tap::Flick),
            tap(t, 3_160, 0.5, Tap::Flick),
        );
        assert!(!pin(t, first, second), "Two quick strokes, as in an X");
        let stroke = tap(t, 4_000, 0.6, Tap::Flick);
        let (first, second) = (clean(t, 4_160, 0.62), clean(t, 4_320, 0.62));
        assert!(!pin(t, stroke, first), "A quick stroke and a tap");
        assert!(
            pin(t, first, second),
            "A quick stroke costs the double tap after it nothing"
        );
        let (first, second) = (clean(t, 5_000, 0.7), clean(t, 5_160, 0.7));
        tap(t, 5_240, 0.7, Tap::Flick);
        assert!(
            pin(t, first, second),
            "A stroke seen after the taps but before their pin"
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
        assert_eq!(recording.strokes.len(), 4, "The flicks drew");
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
