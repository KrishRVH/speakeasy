use std::{array, f32::consts::FRAC_PI_2, f64::consts::PI, ops::RangeInclusive};

use super::Shape;
use crate::{
    Point, Rect,
    recording::{Size, Stroke},
};

const TAP_PIXELS: f64 = 14.0;

const STRAIGHTNESS: f64 = 0.88;

const UNDERLINE_ASPECT: f64 = 2.5;

const LOOP_CLOSURE: f64 = 0.35;

const LOOP_LENGTH: f64 = 2.0;

const LOOP_FILL: f64 = 0.32;

const CIRCLE_PIXELS: f64 = 18.0;

const ROUND: f64 = 0.42;

const CIRCLE_FLATNESS: f64 = 0.15;
const FLAT_LOOP_CLOSURE: f64 = 0.6;

const FLAT_LOOP_TURNS: f64 = 3.5;

const INNER_LOOP_SHARE: f64 = 0.6;
const INNER_LOOP_CLOSURE: f64 = 0.15;

const INNER_LOOP_FILL: f64 = 0.6;
const INNER_LOOP_TURNS: f64 = 1.5;

const INNER_LOOP_ENDS: usize = 512;

const SLANTS: usize = 12;

const SHAFT_SHARE: f32 = 0.5;

const SHAFT_WOBBLE: f32 = 0.1;

const SHAFT_ARRIVAL: f32 = 0.3;

const HEAD_SHARE: f32 = 0.15;
const HEAD_PIXELS: f32 = 16.0;

const BARB_WOBBLE: f32 = 0.15;

const BARB_REACH: f32 = 0.9;
const LONE_BARB_REACH: f32 = 0.6;

const BARB_SPREAD: f32 = 75.0;
const BARBS_SPREAD: f32 = 5.0;
const LONE_BARB_SPREAD: f32 = 15.0;

const HEAD_BALANCE: f32 = 0.3;

pub(super) struct Ink {
    pub(super) shape: Shape,
    pub(super) region: Rect,
}

pub(super) fn ink(stroke: &Stroke, surface: Size) -> Option<Ink> {
    let points = stroke.points();
    let path: Vec<Point> = stroke
        .path()
        .map(|point| point.scaled(surface.width, surface.height))
        .collect();
    let traced = trace(&path);
    let bounds = Rect::bounding(path.iter().copied())?;
    let whole = Stretch::between(*traced.first()?, *traced.last()?, bounds);
    let inner = || inner_loop(&traced).and_then(|round| points.get(round));
    let (shape, marking) = if whole.width.hypot(whole.height) < TAP_PIXELS {
        (Shape::Tap, points)
    } else if whole.closes(LOOP_CLOSURE) && whole.fills(LOOP_FILL, whole.width * whole.height) {
        (Shape::Circle, inner().unwrap_or(points))
    } else if whole.gap > STRAIGHTNESS * whole.length {
        let straight = if whole.width > UNDERLINE_ASPECT * whole.height {
            Shape::Underline
        } else {
            Shape::Line {
                from: stroke.first().point,
                to: stroke.last().point,
            }
        };
        (straight, points)
    } else if let Some(round) = inner() {
        (Shape::Circle, round)
    } else if let Some(tip) =
        arrow_tip(&path, bounds.width().max(bounds.height())).and_then(|tip| points.get(tip))
    {
        let arrow = Shape::Arrow {
            from: stroke.first().point,
            to: tip.point,
        };
        (arrow, points)
    } else {
        (Shape::Scribble, points)
    };
    Some(Ink {
        shape,
        region: Rect::bounding(marking.iter().map(|ink| ink.point))?,
    })
}

#[derive(Clone, Copy)]
struct Traced {
    point: Point,
    length: f64,
    swept: f64,
}

fn trace(path: &[Point]) -> Vec<Traced> {
    let Some(&first) = path.first() else {
        return Vec::new();
    };
    let origin = Traced {
        point: first,
        length: 0.0,
        swept: 0.0,
    };
    path.iter()
        .scan(origin, |last, &point| {
            *last = Traced {
                point,
                length: last.length + f64::from(last.point.distance(point)),
                swept: last.swept + cross(last.point, point),
            };
            Some(*last)
        })
        .collect()
}

fn cross(one: Point, other: Point) -> f64 {
    f64::from(one.x) * f64::from(other.y) - f64::from(other.x) * f64::from(one.y)
}

struct Stretch {
    width: f64,
    height: f64,
    length: f64,
    gap: f64,
    area: f64,
}

impl Stretch {
    fn between(start: Traced, end: Traced, bounds: Rect) -> Self {
        Self {
            width: f64::from(bounds.width()),
            height: f64::from(bounds.height()),
            length: end.length - start.length,
            gap: f64::from(start.point.distance(end.point)),
            area: 0.5 * (end.swept - start.swept + cross(end.point, start.point)).abs(),
        }
    }

    fn closes(&self, closure: f64) -> bool {
        let (long, short) = self.sides();
        self.gap < closure * long
            && self.length > LOOP_LENGTH * long
            && short >= CIRCLE_PIXELS
            && (short > ROUND * long
                || (short > CIRCLE_FLATNESS * long
                    && self.gap < FLAT_LOOP_CLOSURE * short
                    && self.turns() <= FLAT_LOOP_TURNS))
    }

    fn fills(&self, share: f64, boxed: f64) -> bool {
        self.area > share * boxed
    }

    fn sides(&self) -> (f64, f64) {
        (self.width.max(self.height), self.width.min(self.height))
    }

    fn opening(&self) -> f64 {
        let (long, _) = self.sides();
        self.gap / long
    }

    fn turns(&self) -> f64 {
        let (width, height) = (self.width, self.height);
        let perimeter = PI / 2.0
            * (3.0 * (width + height) - ((3.0 * width + height) * (width + 3.0 * height)).sqrt());
        self.length / perimeter
    }
}

type Slanted = [Point; SLANTS];

struct Boxes([Rect; SLANTS]);

impl Boxes {
    fn at(point: &Slanted) -> Self {
        Self(point.map(Rect::at))
    }

    fn hold(&mut self, point: &Slanted) {
        for (bounds, &point) in self.0.iter_mut().zip(point) {
            *bounds = bounds.union(Rect::at(point));
        }
    }

    fn tightest(&self) -> f64 {
        self.0
            .iter()
            .map(|bounds| f64::from(bounds.width()) * f64::from(bounds.height()))
            .fold(f64::INFINITY, f64::min)
    }
}

fn inner_loop(path: &[Traced]) -> Option<RangeInclusive<usize>> {
    let last = path.len().checked_sub(1)?;
    let total = path.get(last)?.length;
    let steps = last.min(INNER_LOOP_ENDS.saturating_sub(1));
    let slants: [Point; SLANTS] = array::from_fn(|slant| {
        let (sin, cos) = (FRAC_PI_2 * slant as f32 / SLANTS as f32).sin_cos();
        Point::new(cos, sin)
    });
    let ends: Vec<(usize, Traced, Slanted)> = (0..=steps)
        .filter_map(|step| {
            let index = step.checked_mul(last)?.checked_div(steps)?;
            let end = *path.get(index)?;
            let slanted = slants.map(|along| {
                Point::new(end.point.dot(along), end.point.dot(along.perpendicular()))
            });
            Some((index, end, slanted))
        })
        .collect();

    let latest = (1.0 - INNER_LOOP_SHARE) * total;
    ends.iter()
        .enumerate()
        .take_while(|&(_, &(_, start, _))| start.length <= latest)
        .flat_map(|(tried, &(from, start, ref slanted))| {
            let mut bounds = Rect::at(start.point);

            let mut boxes = Boxes::at(slanted);
            let mut unheld = ends.iter().enumerate().skip(tried).peekable();
            ends.iter()
                .enumerate()
                .skip(tried)
                .filter_map(move |(at, &(to, end, _))| {
                    bounds = bounds.union(Rect::at(end.point));
                    let stretch = Stretch::between(start, end, bounds);
                    let may_go_round = stretch.length >= INNER_LOOP_SHARE * total
                        && stretch.turns() <= INNER_LOOP_TURNS
                        && stretch.closes(INNER_LOOP_CLOSURE);
                    if !may_go_round {
                        return None;
                    }
                    while let Some((_, (_, _, slanted))) = unheld.next_if(|&(held, _)| held <= at) {
                        boxes.hold(slanted);
                    }
                    let goes_round = stretch.fills(INNER_LOOP_FILL, boxes.tightest());
                    goes_round.then(|| (stretch.opening(), from..=to))
                })
        })
        .min_by(|(one, _), (other, _)| one.total_cmp(other))
        .map(|(_, round)| round)
}

fn arrow_tip(path: &[Point], size: f32) -> Option<usize> {
    let turn = turning_point(path, size)?;
    let shaft = path.get(..=turn)?;
    let head = path.get(turn..)?;
    let (&start, &tip) = (shaft.first()?, shaft.last()?);
    let length = start.distance(tip);
    if shaft
        .iter()
        .any(|&point| off_line(point, start, tip) > SHAFT_WOBBLE * length)
    {
        return None;
    }
    let forward = arrival(shaft)?;
    let slack = head_size(length);
    if head.iter().any(|&point| (point - tip).dot(forward) > slack) {
        return None;
    }

    let measured: Vec<(f32, f32)> = barbs(head, slack)
        .into_iter()
        .map(|barb| {
            let barb = head.get(barb)?;
            let (&from, &end) = (barb.first()?, barb.last()?);
            let straight = barb
                .iter()
                .all(|&point| off_line(point, from, end) <= BARB_WOBBLE * from.distance(end));
            let reach = (end - tip).length();
            let spread = (-(end - tip).dot(forward) / reach)
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees();
            (from.distance(tip) <= slack
                && straight
                && reach <= BARB_REACH * length
                && spread <= BARB_SPREAD)
                .then_some((reach, spread))
        })
        .collect::<Option<_>>()?;
    let pointed = match measured[..] {
        [(reach, spread)] => reach <= LONE_BARB_REACH * length && spread >= LONE_BARB_SPREAD,
        [(_, one), (_, other)] => one.min(other) >= BARBS_SPREAD,
        _ => false,
    };
    let across = forward.perpendicular();
    let (left, right) = head
        .iter()
        .map(|&point| (point - tip).dot(across))
        .fold((0.0_f32, 0.0_f32), |(left, right), side| {
            (left.max(side), right.max(-side))
        });
    (pointed && left.min(right) > HEAD_BALANCE * left.max(right)).then_some(turn)
}

fn turning_point(path: &[Point], size: f32) -> Option<usize> {
    let &start = path.first()?;
    let (mut tip, mut farthest) = (0, 0.0);
    for (index, &point) in path.iter().enumerate() {
        let reach = start.distance(point);
        if reach >= farthest {
            (tip, farthest) = (index, reach);
        } else if farthest >= SHAFT_SHARE * size && reach < farthest - head_size(farthest) {
            return Some(tip);
        }
    }
    None
}

fn head_size(length: f32) -> f32 {
    (HEAD_SHARE * length).min(HEAD_PIXELS)
}

fn arrival(shaft: &[Point]) -> Option<Point> {
    let (&start, &tip) = (shaft.first()?, shaft.last()?);
    let length: f32 = shaft
        .array_windows()
        .map(|&[one, next]| one.distance(next))
        .sum();
    let mut run = 0.0;
    let back = shaft
        .array_windows()
        .find(|&&[one, next]| {
            run += one.distance(next);
            run >= (1.0 - SHAFT_ARRIVAL) * length
        })
        .map_or(start, |&[one, _]| one);
    let along = back.distance(tip);
    (along >= 1.0).then(|| (tip - back) * along.recip())
}

fn barbs(head: &[Point], slack: f32) -> Vec<RangeInclusive<usize>> {
    let Some(&tip) = head.first() else {
        return Vec::new();
    };
    let mut barbs = Vec::new();
    let (mut from, mut nearest) = (0, 0.0);
    let (mut to, mut farthest) = (0, 0.0);
    let mut outward = true;
    for (index, &point) in head.iter().enumerate() {
        let reach = tip.distance(point);
        if outward {
            if reach >= farthest {
                (to, farthest) = (index, reach);
            } else if reach < farthest - slack {
                barbs.push(from..=to);
                (from, nearest, outward) = (index, reach, false);
            }
        } else if reach <= nearest {
            (from, nearest) = (index, reach);
        } else if reach > nearest + slack {
            (to, farthest, outward) = (index, reach, true);
        }
    }
    if outward && farthest >= slack {
        barbs.push(from..=to);
    }
    barbs
}

fn off_line(point: Point, one: Point, other: Point) -> f32 {
    let along = other - one;
    (point - one).dot(along.perpendicular()).abs() / along.length()
}
