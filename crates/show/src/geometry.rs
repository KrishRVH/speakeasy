use std::ops::{Add, Mul, Sub};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "[f32; 2]", into = "[f32; 2]")]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    #[must_use]
    pub(crate) fn length(self) -> f32 {
        self.x.hypot(self.y)
    }

    #[must_use]
    pub(crate) fn distance(self, other: Self) -> f32 {
        (self - other).length()
    }

    #[must_use]
    pub(crate) fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }

    #[must_use]
    pub(crate) fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y
    }

    #[must_use]
    pub(crate) const fn perpendicular(self) -> Self {
        Self::new(self.y, -self.x)
    }

    #[must_use]
    pub(crate) fn rotated(self, center: Self, angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        let offset = self - center;
        center
            + Self::new(
                offset.x * cos - offset.y * sin,
                offset.x * sin + offset.y * cos,
            )
    }

    #[must_use]
    pub fn scaled(self, x: f32, y: f32) -> Self {
        Self::new(self.x * x, self.y * y)
    }

    #[must_use]
    pub(crate) const fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl Add for Point {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

impl Sub for Point {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}

impl Mul<f32> for Point {
    type Output = Self;

    fn mul(self, factor: f32) -> Self {
        Self::new(self.x * factor, self.y * factor)
    }
}

impl From<[f32; 2]> for Point {
    fn from([x, y]: [f32; 2]) -> Self {
        Self::new(x, y)
    }
}

impl From<Point> for [f32; 2] {
    fn from(point: Point) -> Self {
        [point.x, point.y]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "[f32; 4]", into = "[f32; 4]")]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub(crate) const UNIT: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    #[must_use]
    pub(crate) fn from_origin(origin: Point, width: f32, height: f32) -> Self {
        Self {
            left: origin.x,
            top: origin.y,
            right: origin.x + width,
            bottom: origin.y + height,
        }
    }

    #[must_use]
    pub(crate) const fn at(point: Point) -> Self {
        Self {
            left: point.x,
            top: point.y,
            right: point.x,
            bottom: point.y,
        }
    }

    #[must_use]
    pub(crate) fn bounding(points: impl IntoIterator<Item = Point>) -> Option<Self> {
        points.into_iter().map(Self::at).reduce(Self::union)
    }

    #[must_use]
    pub(crate) fn width(self) -> f32 {
        self.right - self.left
    }

    #[must_use]
    pub(crate) fn height(self) -> f32 {
        self.bottom - self.top
    }

    #[must_use]
    pub(crate) fn center(self) -> Point {
        Point::new(
            self.left.midpoint(self.right),
            self.top.midpoint(self.bottom),
        )
    }

    #[must_use]
    pub(crate) fn contains(self, point: Point) -> bool {
        (self.left..=self.right).contains(&point.x) && (self.top..=self.bottom).contains(&point.y)
    }

    #[must_use]
    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    #[must_use]
    pub(crate) fn expanded_within(self, margin: f32, bounds: Self) -> Self {
        Self {
            left: (self.left - margin).max(bounds.left),
            top: (self.top - margin).max(bounds.top),
            right: (self.right + margin).min(bounds.right),
            bottom: (self.bottom + margin).min(bounds.bottom),
        }
    }
}

impl From<[f32; 4]> for Rect {
    fn from([left, top, right, bottom]: [f32; 4]) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
}

impl From<Rect> for [f32; 4] {
    fn from(rect: Rect) -> Self {
        [rect.left, rect.top, rect.right, rect.bottom]
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use super::*;

    fn near(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-5
    }

    #[test]
    fn rotation_follows_screen_orientation() {
        let center = Point::new(1.0, 1.0);
        assert!(near(
            Point::new(2.0, 1.0).rotated(center, FRAC_PI_2),
            Point::new(1.0, 2.0)
        ));
        assert!(near(
            Point::new(3.0, 4.0).perpendicular(),
            Point::new(4.0, -3.0)
        ));
    }

    #[test]
    fn bounds_cover_every_point() {
        assert_eq!(Rect::bounding([]), None);
        let points = [
            Point::new(0.4, 0.9),
            Point::new(0.1, 0.5),
            Point::new(0.7, 0.2),
        ];
        let bounds = Rect::bounding(points).unwrap();
        assert_eq!(<[f32; 4]>::from(bounds), [0.1, 0.2, 0.7, 0.9]);
        assert_eq!(
            <[f32; 4]>::from(bounds.expanded_within(0.25, Rect::UNIT)),
            [0.0, 0.0, 0.95, 1.0]
        );
    }

    #[test]
    fn containment_includes_edges_and_rejects_non_finite_points() {
        let inside = [
            Point::new(0.0, 1.0),
            Point::new(1.0, 0.0),
            Point::new(0.5, 0.5),
        ];
        assert!(inside.into_iter().all(|point| Rect::UNIT.contains(point)));
        let outside = [
            Point::new(-0.01, 0.5),
            Point::new(0.5, 1.01),
            Point::new(f32::NAN, 0.5),
            Point::new(0.5, f32::INFINITY),
        ];
        assert!(!outside.into_iter().any(|point| Rect::UNIT.contains(point)));
    }
}
