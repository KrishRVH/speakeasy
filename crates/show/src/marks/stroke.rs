//! What one stroke looks like in pixels of the surface: a tap, a loop round something, a straight
//! line or underline, an arrow drawn in one go, or a scribble.

use std::{f64::consts::PI, ops::RangeInclusive};

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
/// stroke, its ends meet within [`INNER_LOOP_CLOSURE`] of its long side, and it goes round at most
/// [`INNER_LOOP_TURNS`] times.
const INNER_LOOP_SHARE: f64 = 0.6;
const INNER_LOOP_CLOSURE: f64 = 0.15;
const INNER_LOOP_TURNS: f64 = 1.5;
/// The most points the search for an inner loop tries as its ends, which bounds its quadratic cost.
const INNER_LOOP_ENDS: usize = 512;

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

/// The shape `stroke` makes on a surface `surface` pixels in size.
pub(super) fn shape(stroke: &Stroke, surface: Size) -> Shape {
    let path: Vec<Point> = stroke
        .path()
        .map(|point| point.scaled(surface.width, surface.height))
        .collect();
    let traced = trace(&path);
    let (Some(&start), Some(&end), Some(bounds)) = (
        traced.first(),
        traced.last(),
        Rect::bounding(path.iter().copied()),
    ) else {
        return Shape::Tap;
    };
    let whole = Stretch::between(start, end, bounds);
    if whole.width.hypot(whole.height) < TAP_PIXELS {
        Shape::Tap
    } else if whole.encloses(LOOP_CLOSURE) {
        Shape::Circle
    } else if whole.gap > STRAIGHTNESS * whole.length {
        if whole.width > UNDERLINE_ASPECT * whole.height {
            Shape::Underline
        } else {
            Shape::Line {
                from: stroke.first().point,
                to: stroke.last().point,
            }
        }
    } else if has_inner_loop(&traced) {
        Shape::Circle
    } else if let Some(tip) = arrow_tip(&path, bounds.width().max(bounds.height()))
        .and_then(|tip| stroke.points().get(tip))
    {
        Shape::Arrow {
            from: stroke.first().point,
            to: tip.point,
        }
    } else {
        Shape::Scribble
    }
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
    width: f64,
    height: f64,
    length: f64,
    /// How far apart its ends are.
    gap: f64,
    /// The area it encloses once its ends are joined.
    area: f64,
}

impl Stretch {
    /// The stretch from `start` to `end`, whose points `bounds` holds.
    fn between(start: Traced, end: Traced, bounds: Rect) -> Self {
        Self {
            width: f64::from(bounds.width()),
            height: f64::from(bounds.height()),
            length: end.length - start.length,
            gap: f64::from(start.point.distance(end.point)),
            area: 0.5 * (end.swept - start.swept + cross(end.point, start.point)).abs(),
        }
    }

    /// Whether the stretch goes round something: its ends meet within `closure` of its long side,
    /// it runs [`LOOP_LENGTH`] times that side, encloses [`LOOP_FILL`] of its box, spans
    /// [`CIRCLE_PIXELS`] both ways, and is round or a flat loop as [`CIRCLE_FLATNESS`] describes.
    fn encloses(&self, closure: f64) -> bool {
        let (long, short) = (self.width.max(self.height), self.width.min(self.height));
        self.gap < closure * long
            && self.length > LOOP_LENGTH * long
            && short >= CIRCLE_PIXELS
            && self.area > LOOP_FILL * self.width * self.height
            && (short > ROUND * long
                || (short > CIRCLE_FLATNESS * long
                    && self.gap < FLAT_LOOP_CLOSURE * short
                    && self.turns() <= FLAT_LOOP_TURNS))
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

/// Whether the path goes round something once inside a longer stroke, after a lead-in or before a
/// tail or an overshoot, as [`INNER_LOOP_SHARE`] describes.
///
/// Trying every pair of points as a loop's ends would cost the square of the points, so only
/// every few points are tried, at most [`INNER_LOOP_ENDS`]; each stretch is still judged by every
/// point along it, so sampling smooths no zigzag away and folds no turns together.
fn has_inner_loop(path: &[Traced]) -> bool {
    let Some(total) = path.last().map(|end| end.length) else {
        return false;
    };
    let step = path.len().div_ceil(INNER_LOOP_ENDS);
    let last = path.len().saturating_sub(1);
    let tried = |index: usize| index.is_multiple_of(step) || index == last;
    path.iter()
        .enumerate()
        .filter(|&(index, _)| tried(index))
        .any(|(from, &start)| {
            let mut bounds = Rect::at(start.point);
            path.iter().enumerate().skip(from).any(|(to, &end)| {
                bounds = bounds.union(Rect::at(end.point));
                if !tried(to) || end.length - start.length < INNER_LOOP_SHARE * total {
                    return false;
                }
                let stretch = Stretch::between(start, end, bounds);
                stretch.turns() <= INNER_LOOP_TURNS && stretch.encloses(INNER_LOOP_CLOSURE)
            })
        })
}

/// Where an arrow drawn in one go along `path` points, as the index of its tip, for a path whose
/// box's long side is `size`.
///
/// The shaft runs straight from the first point to the tip, where the pen turns back into one or
/// two straight barbs that end behind the tip, and nothing runs on past it. Two barbs (out, back,
/// and out again) may be long; a lone barb is shorter, and it must spread clearly from the shaft
/// and run on across it, since the head reaches both sides of the shaft. An underline whose end
/// hooks back, a check mark, a V, and a zigzag are not arrows, and neither is an arrow drawn head
/// first.
fn arrow_tip(path: &[Point], size: f32) -> Option<usize> {
    let turn = turning_point(path, size)?;
    let shaft = path.get(..=turn)?;
    let head = path.get(turn..)?;
    let (&start, &tip) = (shaft.first()?, shaft.last()?);
    let length = start.distance(tip);
    if shaft
        .iter()
        .any(|&along| off_line(along, start, tip) > SHAFT_WOBBLE * length)
    {
        return None;
    }
    let forward = arrival(shaft)?;
    let slack = head_size(length);
    if head.iter().any(|&after| (after - tip).dot(forward) > slack) {
        return None;
    }
    // Each barb's reach from the tip, and its spread from the shaft in degrees.
    let barbs: Vec<(f32, f32)> = barbs(head, slack)
        .into_iter()
        .map(|barb| {
            let barb = head.get(barb)?;
            let (&from, &end) = (barb.first()?, barb.last()?);
            let straight = barb
                .iter()
                .all(|&along| off_line(along, from, end) <= BARB_WOBBLE * from.distance(end));
            let reach = (end - tip).length();
            let spread = (-(end - tip).dot(forward) / reach)
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees();
            (straight && reach <= BARB_REACH * length && spread <= BARB_SPREAD)
                .then_some((reach, spread))
        })
        .collect::<Option<_>>()?;
    let pointed = match barbs[..] {
        [(reach, spread)] => reach <= LONE_BARB_REACH * length && spread >= LONE_BARB_SPREAD,
        [(_, one), (_, other)] => one.min(other) >= BARBS_SPREAD,
        _ => false,
    };
    let across = forward.perpendicular();
    let (left, right) = head
        .iter()
        .map(|&after| (after - tip).dot(across))
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
