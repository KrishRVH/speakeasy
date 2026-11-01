use std::f32::consts::{PI, TAU};

use crate::Point;

const THINNING: f32 = 0.5;

const SMOOTHING: f32 = 0.5;

const STREAMLINE: f32 = 0.5;

const PRESSURE_RATE: f32 = 0.275;

const END_JITTER: f32 = 3.0;

const ARC_STEPS: u8 = 13;

pub const COLOR: [u8; 4] = [255, 59, 48, 255];

pub const LABEL_RING: f32 = 0.14;

pub const PIN_DOT: f32 = 0.32;

pub const LABEL_TYPE: f32 = 1.3;

pub const LABEL_DIGITS: f32 = 1.4;

#[must_use]
pub fn pen(width: f32, height: f32) -> f32 {
    (width.min(height) * 0.0043).clamp(2.8, 4.0) * detail(width, height)
}

#[must_use]
pub fn label_radius(width: f32, height: f32) -> f32 {
    12.0 * detail(width, height)
}

pub(crate) fn detail(width: f32, height: f32) -> f32 {
    (width.min(height) / 800.0)
        .max(width.max(height) / 1280.0)
        .clamp(1.0, 2.0)
}

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
    let (first, last) = match nodes.as_slice() {
        [] => return Vec::new(),
        [only] => return dot(only.point, radius(0.25).max(0.01)),
        [first, .., last] => (first, last),
    };
    let length = last.run;

    let simulate = |pressure: f32, step: f32| {
        let speed = (step / size).min(1.0);
        (pressure + (1.0 - speed - pressure) * speed * PRESSURE_RATE).min(1.0)
    };
    let mut pressure = nodes.iter().take(10).fold(0.25, |pressure, node| {
        f32::midpoint(pressure, simulate(pressure, node.step))
    });

    let min_spacing_squared = (size * SMOOTHING).powi(2);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut previous_direction = first.direction;
    let mut rounding = false;
    let mut end_radius = radius(pressure);
    let final_index = nodes.len().saturating_sub(1);
    for (index, node) in nodes.iter().enumerate() {
        let is_last = index == final_index;
        if !is_last && length - node.run < END_JITTER {
            continue;
        }
        let node_pressure = simulate(pressure, node.step);
        let node_radius = radius(node_pressure).max(0.01);
        end_radius = node_radius;
        let next_direction = nodes
            .get(index.saturating_add(1))
            .map_or(node.direction, |next| next.direction);
        let turn = node.direction.dot(next_direction);
        let turns_back = turn < 0.0;
        let sharp_corner =
            (node.direction.dot(previous_direction) < 0.0 && !rounding) || turns_back;
        if sharp_corner {
            let offset = previous_direction.perpendicular() * node_radius;
            for angle in arc(0..=ARC_STEPS, ARC_STEPS) {
                left.push((node.point - offset).rotated(node.point, angle));
                right.push((node.point + offset).rotated(node.point, -angle));
            }
            rounding = turns_back;
            continue;
        }
        rounding = false;
        let along = if is_last {
            node.direction
        } else {
            next_direction.lerp(node.direction, turn)
        };
        let offset = along.perpendicular() * node_radius;
        let forced = is_last || index <= 1;
        for (side, point) in [
            (&mut left, node.point - offset),
            (&mut right, node.point + offset),
        ] {
            let spaced = |&previous: &Point| {
                let gap = previous - point;
                gap.dot(gap) > min_spacing_squared
            };
            if forced || side.last().is_none_or(spaced) {
                side.push(point);
            }
        }
        pressure = node_pressure;
        previous_direction = node.direction;
    }

    let cap = last.point - last.direction.perpendicular() * end_radius;
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

struct Node {
    point: Point,
    direction: Point,
    step: f32,
    run: f32,
}

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
    let mut previous = start;
    let mut run = 0.0;
    let mut started = false;
    let final_index = rest.len().saturating_sub(1);
    for (index, &raw) in rest.iter().enumerate() {
        let is_last = index == final_index;
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
        previous = point;
    }
    if let [first, second, ..] = nodes.as_mut_slice() {
        first.direction = second.direction;
    }
    nodes
}

fn arc(steps: impl Iterator<Item = u8>, of: u8) -> impl Iterator<Item = f32> {
    steps.map(move |step| PI * f32::from(step) / f32::from(of))
}

fn dot(center: Point, radius: f32) -> Vec<Point> {
    (0..32_u8)
        .chain([0])
        .map(|step| {
            let angle = TAU * f32::from(step) / 32.0;
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
