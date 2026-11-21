use std::{collections::VecDeque, mem, time::Duration};

use crate::{
    Action, InkPoint, Mark, Moment, Pin, Point, Recording, Rect, Size, Spaces, Span, Stroke, marks,
    recording::spaces::{self, PinSpace, Space},
};

const DRAG_PIXELS: f32 = 12.0;

const INK_SPACING_PIXELS: f32 = 1.5;

const SHORTEST_STROKE_PIXELS: f32 = 6.0;

const MOST_STROKE_POINTS: usize = 2_400;

const PIN_LOOKBACK: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub struct Tracker {
    surface: Size,
    space: Option<Space>,
    generation: Option<u64>,
    spaces: Spaces,
    seen: VecDeque<Seen>,
    ink: Option<Ink>,
    press: Button,
    pin_after: Option<Moment>,
    shift_down: Option<Moment>,
    shift_uses: Vec<Span>,
    strokes: Vec<Stroke>,
    pins: Vec<Pin>,
    actions: Vec<Action>,
}

#[derive(Clone, Copy, Debug)]
struct Seen {
    point: InkPoint,
    space: Option<Space>,
}

#[derive(Debug)]
struct Ink {
    points: Vec<InkPoint>,
    deliberate: bool,
    space: Option<Space>,
    last_space: Option<Space>,
}

impl Ink {
    fn start(point: InkPoint, space: Option<Space>) -> Self {
        Self {
            points: vec![point],
            deliberate: false,
            space,
            last_space: space,
        }
    }

    fn extend(&mut self, point: InkPoint, space: Option<Space>, surface: Size) {
        if self.space != space {
            self.space = None;
        }
        self.last_space = space;
        self.deliberate |= self.points.first().is_some_and(|first| {
            pixels(surface, first.point, point.point) >= SHORTEST_STROKE_PIXELS
        });
        self.points.push(point);
    }

    fn continuation(&self) -> Option<Self> {
        self.points.last().map(|&joint| Self {
            points: vec![joint],
            deliberate: self.deliberate,
            space: self.last_space,
            last_space: self.last_space,
        })
    }
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
            spaces: Spaces {
                strokes: Vec::new(),
                pins: Vec::new(),
                actions: Vec::new(),
            },
            seen: VecDeque::new(),
            ink: None,
            press: Button::Up,
            pin_after: None,
            shift_down: None,
            shift_uses: Vec::new(),
            strokes: Vec::new(),
            pins: Vec::new(),
            actions: Vec::new(),
        }
    }

    pub(crate) const fn resize(&mut self, surface: Size) {
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

    pub(crate) fn gap(&mut self, at: Moment) {
        self.lift();
        self.space = None;
        self.seen.clear();
        self.press = Button::Unknown;
        self.shift_down = None;
        self.pin_after = Some(self.pin_after.unwrap_or(at).max(at));
    }

    pub fn observe(&mut self, at: Moment, point: Point, drawing: bool, pressed: bool) {
        if self.seen.is_empty()
            && let Some(gap) = &mut self.pin_after
        {
            *gap = (*gap).max(at);
        }
        self.see(InkPoint { point, at });
        let inking = drawing && self.pins.last().is_none_or(|pin| at > pin.at);
        self.shift_down = inking.then(|| self.shift_down.unwrap_or(at));
        if inking && pressed {
            self.use_shift(at);
        }
        let inside = Rect::UNIT.contains(point);
        if inking && inside {
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
        self.ink
            .as_ref()
            .filter(|ink| ink.deliberate)
            .map_or(&[], |ink| &ink.points)
    }

    #[must_use]
    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

    pub(crate) fn pin(&mut self, taps: Span) -> bool {
        let used = self
            .shift_uses
            .iter()
            .rev()
            .take_while(|used| used.end() >= taps.start())
            .any(|used| used.start() <= taps.end());
        let pinned = self.pins.last().is_some_and(|pin| taps.start() <= pin.at);
        if !used
            && !pinned
            && self.pin_after.is_none_or(|gap| taps.start() > gap)
            && let Some(&Seen {
                point:
                    InkPoint {
                        point,
                        at: observed,
                    },
                space,
            }) = self
                .seen
                .iter()
                .rev()
                .find(|seen| seen.point.at <= taps.end())
            && Rect::UNIT.contains(point)
        {
            if self
                .ink
                .take_if(|ink| {
                    !ink.deliberate
                        && ink
                            .points
                            .first()
                            .is_some_and(|first| first.at <= taps.end())
                })
                .is_some()
            {
                self.shift_down = None;
            }
            self.pins.push(Pin {
                at: taps.end(),
                point,
            });
            self.spaces
                .pins
                .push(space.map(|space| PinSpace { space, observed }));
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

    fn draw(&mut self, point: InkPoint) {
        let spaced = self
            .ink
            .as_ref()
            .and_then(|ink| ink.points.last())
            .is_none_or(|last| pixels(self.surface, last.point, point.point) >= INK_SPACING_PIXELS);
        if !spaced {
            return;
        }
        if self
            .ink
            .as_ref()
            .is_some_and(|ink| ink.points.len() >= MOST_STROKE_POINTS)
        {
            self.cut();
        }
        let deliberate = if let Some(ink) = &mut self.ink {
            ink.extend(point, self.space, self.surface);
            ink.deliberate
        } else {
            self.ink = Some(Ink::start(point, self.space));
            false
        };
        if deliberate {
            self.use_shift(point.at);
        }
    }

    fn see(&mut self, point: InkPoint) {
        let horizon = point.at.saturating_sub(PIN_LOOKBACK);
        while self
            .seen
            .get(1)
            .is_some_and(|next| next.point.at <= horizon)
        {
            self.seen.pop_front();
        }
        self.seen.push_back(Seen {
            point,
            space: self.space,
        });
    }

    fn use_shift(&mut self, at: Moment) {
        let Some(down) = self.shift_down else {
            return;
        };
        match self.shift_uses.last_mut() {
            Some(used) if used.start() == down => *used = used.union(Span::at(at)),
            _ => self.shift_uses.push(Span::new(down, at)),
        }
    }

    fn cut(&mut self) {
        if let Some(ink) = self.ink.take() {
            self.ink = ink.continuation();
            self.keep(ink);
        }
    }

    fn lift(&mut self) {
        if let Some(ink) = self.ink.take() {
            self.keep(ink);
        }
    }

    fn keep(&mut self, ink: Ink) {
        if ink.deliberate
            && let Some(stroke) = Stroke::new(ink.points)
        {
            self.strokes.push(stroke);
            self.spaces.strokes.push(ink.space);
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

fn pixels(surface: Size, one: Point, other: Point) -> f32 {
    (one - other).scaled(surface.width, surface.height).length()
}

#[cfg(test)]
mod tests;
