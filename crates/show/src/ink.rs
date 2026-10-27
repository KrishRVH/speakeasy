//! Ink as the overlay and a bundle both draw it: its color, its pen, and the outline of a freehand
//! stroke, as a pen of varying pressure would leave it.
//!
//! The outline is adapted from perfect-freehand 1.2.2 under the MIT License; see `NOTICE.md`.

use std::f32::consts::PI;

use crate::Point;

/// How much speed narrows the line: at one half, a fast stroke is about half as wide.
const THINNING: f32 = 0.5;
/// Outline points closer than this share of the pen size are skipped.
const SMOOTHING: f32 = 0.5;
/// How far each sample is pulled toward the previous one, damping hand jitter.
const STREAMLINE: f32 = 0.5;
/// Steps in each round cap and corner.
const ARC_STEPS: u8 = 13;

/// One accepted sample along the stroke's center line.
struct Node {
    point: Point,
    /// The unit vector back toward the previous node.
    direction: Point,
    /// Distance from the previous node.
    step: f32,
    /// Distance along the line from the first node.
    run: f32,
}

/// The red of ink on the overlay and in a bundle's frames.
pub const COLOR: [u8; 4] = [255, 59, 48, 255];

/// The pen width for ink on a surface `width × height` pixels, alike on the overlay and in a
/// bundle's frames.
///
/// It is 6 pixels on a 1080p display: thin enough to leave the text a mark points at readable, and
/// thick enough that a fast stroke, which narrows to half, still shows on a contact sheet that
/// scales frames down threefold.
#[must_use]
pub fn pen(width: f32, height: f32) -> f32 {
    (width.min(height) * 0.0043).clamp(2.8, 4.0) * detail(width, height)
}

/// How much a surface `width × height` pixels scales marks up from a laptop screen's, so they keep
/// their weight on large displays.
pub(crate) fn detail(width: f32, height: f32) -> f32 {
    (width.min(height) / 800.0)
        .max(width.max(height) / 1280.0)
        .clamp(1.0, 2.0)
}

/// The closed outline, in the coordinates of `points`, of a stroke drawn through them with a pen
/// `size` wide, narrowing where it moved fast and rounded at its ends and sharp turns.
///
/// Returns no points for an empty path or a size that is not positive and finite.
#[must_use]
pub fn outline(points: &[Point], size: f32) -> Vec<Point> {
    if !size.is_finite() || size <= 0.0 {
        return Vec::new();
    }
    let mut input: Vec<Point> = points
        .iter()
        .copied()
        .filter(|point| point.is_finite())
        .collect();
    input.dedup();
    if let [start, end] = *input.as_slice() {
        input = (0..=4_u8)
            .map(|step| start.lerp(end, f32::from(step) / 4.0))
            .collect();
    }
    let radius = |pressure: f32| size * (0.5 + THINNING * (pressure - 0.5));
    let nodes = nodes(&input, size);
    let (Some(first), Some(last)) = (nodes.first(), nodes.last()) else {
        return Vec::new();
    };
    if nodes.len() == 1 {
        return dot(first.point, radius(0.25).max(0.01));
    }
    let length = last.run;

    // Pressure follows speed: slow movement presses harder and draws wider.
    let simulate = |pressure: f32, step: f32| {
        let speed = (step / size).min(1.0);
        (pressure + (1.0 - speed - pressure) * speed * 0.275).min(1.0)
    };
    let mut pressure = nodes.iter().take(10).fold(0.25, |pressure, node| {
        f32::midpoint(pressure, simulate(pressure, node.step))
    });

    let min_spacing = (size * SMOOTHING).powi(2);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut previous_direction = first.direction;
    let (mut previous_left, mut previous_right) = (first.point, first.point);
    let mut rounding = false;
    let mut end_radius = radius(pressure);
    let final_index = nodes.len().saturating_sub(1);
    for (index, node) in nodes.iter().enumerate() {
        let is_last = index == final_index;
        if !is_last && length - node.run < 3.0 {
            continue;
        }
        let node_pressure = simulate(pressure, node.step);
        let width = radius(node_pressure).max(0.01);
        end_radius = width;
        let next = nodes
            .get(index.saturating_add(1))
            .map_or(node.direction, |next| next.direction);
        let turn = node.direction.dot(next);
        let turns_back = turn < 0.0;
        if (node.direction.dot(previous_direction) < 0.0 && !rounding) || turns_back {
            // A sharp corner gets a round joint instead of a spike.
            let offset = previous_direction.perpendicular() * width;
            for angle in arc(0..=ARC_STEPS, ARC_STEPS) {
                previous_left = (node.point - offset).rotated(node.point, angle);
                previous_right = (node.point + offset).rotated(node.point, -angle);
                left.push(previous_left);
                right.push(previous_right);
            }
            rounding = turns_back;
            continue;
        }
        rounding = false;
        let along = if is_last {
            node.direction
        } else {
            next.lerp(node.direction, turn)
        };
        let offset = along.perpendicular() * width;
        let (on_left, on_right) = (node.point - offset, node.point + offset);
        if is_last
            || index <= 1
            || (previous_left - on_left).dot(previous_left - on_left) > min_spacing
        {
            left.push(on_left);
            previous_left = on_left;
        }
        if is_last
            || index <= 1
            || (previous_right - on_right).dot(previous_right - on_right) > min_spacing
        {
            right.push(on_right);
            previous_right = on_right;
        }
        pressure = node_pressure;
        previous_direction = node.direction;
    }

    // Round caps join the two sides at each end.
    let cap = last.point + (last.direction * -1.0).perpendicular() * end_radius;
    left.extend(
        arc(1..=ARC_STEPS, ARC_STEPS.saturating_add(1)).map(|angle| cap.rotated(last.point, angle)),
    );
    let start_right = right.first().copied();
    left.extend(right.into_iter().rev());
    if let Some(start_right) = start_right {
        left.extend(
            arc(1..=ARC_STEPS, ARC_STEPS).map(|angle| start_right.rotated(first.point, angle)),
        );
    }
    left
}

/// The streamlined center line: each point pulled toward the last, and the first pen-width of
/// travel skipped except for its last point, so the stroke's start does not wobble.
fn nodes(input: &[Point], size: f32) -> Vec<Node> {
    let pull = 0.15 + (1.0 - STREAMLINE) * 0.85;
    let Some((&start, rest)) = input.split_first() else {
        return Vec::new();
    };
    let mut nodes = vec![Node {
        point: start,
        direction: Point::default(),
        step: 0.0,
        run: 0.0,
    }];
    let mut run = 0.0;
    let mut started = false;
    let final_index = rest.len().saturating_sub(1);
    for (index, &raw) in rest.iter().enumerate() {
        let is_last = index == final_index;
        let previous = nodes.last().map_or(start, |node| node.point);
        let point = if is_last {
            raw
        } else {
            previous.lerp(raw, pull)
        };
        let step = previous.distance(point);
        if step <= f32::EPSILON {
            continue;
        }
        run += step;
        if !is_last && !started {
            if run < size {
                continue;
            }
            started = true;
        }
        nodes.push(Node {
            point,
            direction: (previous - point) * step.recip(),
            step,
            run,
        });
    }
    // The first node has no predecessor; it borrows the second node's direction.
    if let Some(second) = nodes.get(1).map(|node| node.direction)
        && let Some(first) = nodes.first_mut()
    {
        first.direction = second;
    }
    nodes
}

/// Angles from zero to π over `steps` steps, at the given step numbers.
fn arc(steps: impl Iterator<Item = u8>, of: u8) -> impl Iterator<Item = f32> {
    steps.map(move |step| PI * f32::from(step) / f32::from(of))
}

/// A closed circle of `radius` around `center`.
fn dot(center: Point, radius: f32) -> Vec<Point> {
    (0..=32_u8)
        .map(|step| {
            let angle = 2.0 * PI * f32::from(step % 32) / 32.0;
            center + Point::new(radius * angle.cos(), radius * angle.sin())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rect;

    #[test]
    fn outlines_are_finite_and_enclose_their_path() {
        assert_eq!(outline(&[], 10.0), Vec::<Point>::new());
        assert_eq!(outline(&[Point::default()], 0.0), Vec::<Point>::new());
        for points in [
            vec![Point::default()],
            vec![Point::default(); 4],
            vec![Point::default(), Point::new(100.0, 0.0)],
            vec![
                Point::default(),
                Point::new(50.0, 0.0),
                Point::new(1.0, 1.0),
            ],
        ] {
            let shape = outline(&points, 10.0);
            assert!(shape.len() > 10, "{}", shape.len());
            assert!(shape.iter().all(|point| point.is_finite()));
        }
        let line = outline(&[Point::default(), Point::new(100.0, 0.0)], 10.0);
        let bounds = Rect::bounding(line).unwrap();
        assert!(bounds.left < -2.0 && bounds.right > 102.0, "{bounds:?}");
        assert!(bounds.top < -2.0 && bounds.bottom > 2.0, "{bounds:?}");
        assert!(bounds.top > -6.0 && bounds.bottom < 6.0, "{bounds:?}");
    }

    #[test]
    fn a_reversal_is_rounded_rather_than_spiked() {
        let hairpin = [
            Point::default(),
            Point::new(60.0, 0.0),
            Point::new(0.0, 1.0),
        ];
        let bounds = Rect::bounding(outline(&hairpin, 8.0)).unwrap();
        assert!(bounds.right < 70.0, "{bounds:?}");
    }
}
