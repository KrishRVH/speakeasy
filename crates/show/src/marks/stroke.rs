//! What one stroke looks like in pixels of the surface: a tap, a loop round something, a straight
//! line or underline, an arrow drawn in one go, or a scribble.

use std::{array, f32::consts::FRAC_PI_2, f64::consts::PI, ops::RangeInclusive};

use super::Shape;
use crate::{
    Point, Rect,
    recording::{Size, Stroke},
};

/// A stroke whose extent is below this many pixels is a tap.
const TAP_PIXELS: f64 = 14.0;
/// A stroke is straight when its ends lie at least this share of its path length apart.
const STRAIGHTNESS: f64 = 0.88;
/// A straight stroke at least this many times wider than tall is an underline.
const UNDERLINE_ASPECT: f64 = 2.5;

/// A loop's ends meet within this share of its long side.
const LOOP_CLOSURE: f64 = 0.35;
/// A loop's path runs at least this many times its long side.
const LOOP_LENGTH: f64 = 2.0;
/// A loop encloses at least this share of its bounding box.
const LOOP_FILL: f64 = 0.32;
/// A circle is at least this many pixels across both ways.
const CIRCLE_PIXELS: f64 = 18.0;
/// A loop whose short side is over this share of its long side is round.
const ROUND: f64 = 0.42;
/// A flatter loop's short side is at least this share of its long side, since people circle wide
/// controls and lines of text with flat loops. A scribble back and forth over a line of text also
/// closes on itself and fills its box, so a flat loop must also close within
/// [`FLAT_LOOP_CLOSURE`] of its short side and go round at most [`FLAT_LOOP_TURNS`] times.
const CIRCLE_FLATNESS: f64 = 0.15;
const FLAT_LOOP_CLOSURE: f64 = 0.6;
/// Three times round, with slack for a hand's jitter.
const FLAT_LOOP_TURNS: f64 = 3.5;
/// A loop inside a stroke with a lead-in, a tail, or an overshoot runs at least this share of the
/// stroke, its ends meet within [`INNER_LOOP_CLOSURE`] of its long side, it encloses
/// [`INNER_LOOP_FILL`] of the tightest of its [`Boxes`], and it goes round at most
/// [`INNER_LOOP_TURNS`] times.
const INNER_LOOP_SHARE: f64 = 0.6;
const INNER_LOOP_CLOSURE: f64 = 0.15;
/// More than a triangle can, since an X drawn in one go closes a triangle where it crosses itself:
/// no triangle fills more than half of any box round it, or a little more once the closing slack
/// trims a corner. An ellipse fills π/4 of the box along its axes, and one no flatter than
/// [`CIRCLE_FLATNESS`] allows fills over 0.7 of a box turned up to 3.75° from them.
const INNER_LOOP_FILL: f64 = 0.6;
const INNER_LOOP_TURNS: f64 = 1.5;
/// The most points the search for an inner loop tries as its ends, which bounds its quadratic cost.
const INNER_LOOP_ENDS: usize = 512;
/// The search boxes a stretch at this many slants spread evenly through a right angle from the
/// surface's axes, so that one box lies within 3.75° of any loop's own axes.
const SLANTS: usize = 12;

/// An arrow drawn in one go turns back at its tip only once this share of the stroke's long side
/// from its start.
const SHAFT_SHARE: f32 = 0.5;
/// The shaft strays at most this share of its length from the line from its start to the tip.
const SHAFT_WOBBLE: f32 = 0.1;
/// The shaft's direction at the tip is taken over this last share of its length.
const SHAFT_ARRIVAL: f32 = 0.3;
/// To turn at the tip, the pen comes back this share of the way it came, and each barb runs as
/// far; never more than [`HEAD_PIXELS`] is needed, so that a small head on a long shaft counts.
const HEAD_SHARE: f32 = 0.15;
const HEAD_PIXELS: f32 = 16.0;
/// A barb strays at most this share of its length from straight.
const BARB_WOBBLE: f32 = 0.15;
/// A barb ends within this share of the shaft from the tip, a lone barb within
/// [`LONE_BARB_REACH`].
const BARB_REACH: f32 = 0.9;
const LONE_BARB_REACH: f32 = 0.6;
/// A barb spreads from the shaft by at most this many degrees, two barbs each by at least
/// [`BARBS_SPREAD`], and a lone barb by at least [`LONE_BARB_SPREAD`].
const BARB_SPREAD: f32 = 75.0;
const BARBS_SPREAD: f32 = 5.0;
const LONE_BARB_SPREAD: f32 = 15.0;
/// A head reaches at least this share as far to one side of the shaft as to the other. Real
/// underlines whose end hooks back to one side read as one-barbed arrows without it.
const HEAD_BALANCE: f32 = 0.3;

/// What one stroke looks like, and the region it marks.
pub(super) struct Ink {
    pub(super) shape: Shape,
    /// Normalized: the box of a loop found inside a longer stroke, without its lead-in or tail, and
    /// otherwise of the whole stroke.
    pub(super) region: Rect,
}

/// What `stroke` looks like on a surface `surface` pixels in size, and the region it marks.
pub(super) fn ink(stroke: &Stroke, surface: Size) -> Option<Ink> {
    let points = stroke.points();
    let path: Vec<Point> = stroke
        .path()
        .map(|point| point.scaled(surface.width, surface.height))
        .collect();
    let traced = trace(&path);
    let bounds = Rect::bounding(path.iter().copied())?;
    let whole = Stretch::between(*traced.first()?, *traced.last()?, bounds);
    let (shape, marking) = if whole.width.hypot(whole.height) < TAP_PIXELS {
        (Shape::Tap, points)
    } else if whole.closes(LOOP_CLOSURE) && whole.fills(LOOP_FILL, whole.width * whole.height) {
        (Shape::Circle, points)
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
    } else if let Some(round) = inner_loop(&traced).and_then(|round| points.get(round)) {
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

/// A point along a path, with the path's length up to it and twice the signed area its segments
/// sweep about the origin, so that any stretch of the path measures in constant time. In f64,
/// since a stretch's measures are differences of sums that grow along the path.
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

/// The cross product of two points as vectors from the origin.
fn cross(one: Point, other: Point) -> f64 {
    f64::from(one.x) * f64::from(other.y) - f64::from(other.x) * f64::from(one.y)
}

/// What a stretch of a path measures, in pixels.
struct Stretch {
    /// Its box along the surface's axes.
    width: f64,
    height: f64,
    length: f64,
    /// How far apart its ends are.
    gap: f64,
    /// The area it encloses once its ends are joined.
    area: f64,
}

impl Stretch {
    /// The stretch from `start` to `end`, taking `bounds` as its box.
    fn between(start: Traced, end: Traced, bounds: Rect) -> Self {
        Self {
            width: f64::from(bounds.width()),
            height: f64::from(bounds.height()),
            length: end.length - start.length,
            gap: f64::from(start.point.distance(end.point)),
            area: 0.5 * (end.swept - start.swept + cross(end.point, start.point)).abs(),
        }
    }

    /// Whether the stretch closes as a loop does: its ends meet within `closure` of its long side,
    /// it runs [`LOOP_LENGTH`] times that side, spans [`CIRCLE_PIXELS`] both ways, and is round or
    /// a flat loop as [`CIRCLE_FLATNESS`] describes.
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

    /// Whether the stretch encloses more than `share` of a box `boxed` square pixels in area.
    fn fills(&self, share: f64, boxed: f64) -> bool {
        self.area > share * boxed
    }

    /// Its long side and its short side.
    fn sides(&self) -> (f64, f64) {
        (self.width.max(self.height), self.width.min(self.height))
    }

    /// How far apart its ends are for its size: their gap as a share of its long side.
    fn opening(&self) -> f64 {
        let (long, _) = self.sides();
        self.gap / long
    }

    /// How many times the stretch would go round the ellipse filling its box, by Ramanujan's
    /// approximation of that ellipse's perimeter.
    fn turns(&self) -> f64 {
        let (width, height) = (self.width, self.height);
        let perimeter = PI / 2.0
            * (3.0 * (width + height) - ((3.0 * width + height) * (width + 3.0 * height)).sqrt());
        self.length / perimeter
    }
}

/// A point's coordinates in the frame of each of [`SLANTS`] slants: along the slant and across it.
type Slanted = [Point; SLANTS];

/// The boxes round a stretch at each of [`SLANTS`] slants, each in its slant's frame, so that one
/// fits a slanted loop nearly as closely as the box along the loop's own axes.
struct Boxes([Rect; SLANTS]);

impl Boxes {
    /// The boxes round a lone point.
    fn at(point: &Slanted) -> Self {
        Self(point.map(Rect::at))
    }

    /// Grows each box to hold `point`.
    fn hold(&mut self, point: &Slanted) {
        for (bounds, &point) in self.0.iter_mut().zip(point) {
            *bounds = bounds.union(Rect::at(point));
        }
    }

    /// The area of the tightest box, in square pixels.
    fn tightest(&self) -> f64 {
        self.0
            .iter()
            .map(|bounds| f64::from(bounds.width()) * f64::from(bounds.height()))
            .fold(f64::INFINITY, f64::min)
    }
}

/// The points of `path` that go round something once inside a longer stroke, after a lead-in or
/// before a tail or an overshoot, as [`INNER_LOOP_SHARE`] describes; of several such stretches, the
/// one whose ends meet closest for its size.
///
/// Trying every pair of points as a loop's ends would cost the square of the points, so at most
/// [`INNER_LOOP_ENDS`] are tried, spread evenly from the first point to the last, and a stretch's
/// boxes hold the tried points along it. Its length and area still count every point along it, so
/// sampling smooths no zigzag away.
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
    // No stretch from a later start runs the loop's share of the stroke.
    let latest = (1.0 - INNER_LOOP_SHARE) * total;
    ends.iter()
        .enumerate()
        .take_while(|&(_, &(_, start, _))| start.length <= latest)
        .flat_map(|(tried, &(from, start, ref slanted))| {
            let mut bounds = Rect::at(start.point);
            // Slanted boxes cost more to grow, so they hold the points up to an end only once the
            // stretch to it may go round.
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

/// Where an arrow drawn in one go along `path` points, as the index of its tip, for a path whose
/// box's long side is `size`.
///
/// The shaft runs straight from the first point to the tip, where the pen turns back into one or
/// two straight barbs, each drawn out from the tip to end behind it, and nothing runs on past the
/// tip. Two barbs (out, back, and out again) may be long; a lone barb is shorter, and it must
/// spread clearly from the shaft and run on across it, since the head reaches both sides of the
/// shaft. An underline whose end hooks back, a check mark, a V, a zigzag, and an X are not arrows,
/// and neither is an arrow drawn head first.
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
    // Each barb's reach from the tip, and its spread from the shaft in degrees.
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

/// Where the pen first heads back toward where it started, once [`SHAFT_SHARE`] of `size` away:
/// the index of the farthest point before it turns.
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

/// How far the pen comes back from a reach `length` pixels long to turn, and how far a barb runs.
fn head_size(length: f32) -> f32 {
    (HEAD_SHARE * length).min(HEAD_PIXELS)
}

/// The unit direction in which `shaft` arrives at its last point, over its last
/// [`SHAFT_ARRIVAL`]; `None` if that is too short to have one.
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

/// The barbs of the head drawn from its tip, `head[0]`: each run in which the pen moves out from
/// the tip by at least `slack` and then back by as much, or lifts, from the point nearest the tip
/// before it to the farthest.
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

/// How far `point` lies from the line through `one` and `other`.
fn off_line(point: Point, one: Point, other: Point) -> f32 {
    let along = other - one;
    (point - one).dot(along.perpendicular()).abs() / along.length()
}
