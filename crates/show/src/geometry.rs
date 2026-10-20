//! Plane geometry shared by ink, marks, and drawing.

use std::ops::{Add, Mul, Sub};

use serde::{Deserialize, Serialize};

/// A point or vector in the plane.
///
/// Recordings store points normalized to the captured surface, so `0.0..=1.0` spans it on both
/// axes; drawing scales them to pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "[f32; 2]", into = "[f32; 2]")]
pub struct Point {
    /// Distance from the left edge.
    pub x: f32,
    /// Distance from the top edge.
    pub y: f32,
}

impl Point {
    /// The point at `(x, y)`.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Euclidean length of the vector.
    #[must_use]
    pub(crate) fn length(self) -> f32 {
        self.x.hypot(self.y)
    }

    /// Euclidean distance to `other`.
    #[must_use]
    pub(crate) fn distance(self, other: Self) -> f32 {
        (self - other).length()
    }

    /// The point `t` of the way from `self` to `other`.
    #[must_use]
    pub(crate) fn lerp(self, other: Self, t: f32) -> Self {
        Self::new(
            (other.x - self.x).mul_add(t, self.x),
            (other.y - self.y).mul_add(t, self.y),
        )
    }

    /// The dot product of two vectors.
    #[must_use]
    pub(crate) fn dot(self, other: Self) -> f32 {
        self.x.mul_add(other.x, self.y * other.y)
    }

    /// The vector turned a quarter clockwise in screen coordinates.
    #[must_use]
    pub(crate) const fn perpendicular(self) -> Self {
        Self::new(self.y, -self.x)
    }

    /// The point turned `angle` radians about `center`.
    #[must_use]
    pub(crate) fn rotated(self, center: Self, angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        let offset = self - center;
        center
            + Self::new(
                offset.x.mul_add(cos, -offset.y * sin),
                offset.x.mul_add(sin, offset.y * cos),
            )
    }

    /// Scales each axis separately, such as from normalized coordinates to pixels.
    #[must_use]
    pub fn scaled(self, x: f32, y: f32) -> Self {
        Self::new(self.x * x, self.y * y)
    }

    /// Whether both coordinates are finite.
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

/// An axis-aligned rectangle, stored by its edges.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "[f32; 4]", into = "[f32; 4]")]
pub struct Rect {
    /// The left edge.
    pub left: f32,
    /// The top edge.
    pub top: f32,
    /// The right edge.
    pub right: f32,
    /// The bottom edge.
    pub bottom: f32,
}

impl Rect {
    /// The whole normalized surface.
    pub(crate) const UNIT: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    /// The rectangle with its top-left corner at `origin`.
    #[must_use]
    pub(crate) fn from_origin(origin: Point, width: f32, height: f32) -> Self {
        Self {
            left: origin.x,
            top: origin.y,
            right: origin.x + width,
            bottom: origin.y + height,
        }
    }

    /// The smallest rectangle holding every point, or `None` without points.
    #[must_use]
    pub(crate) fn bounding(points: impl IntoIterator<Item = Point>) -> Option<Self> {
        points
            .into_iter()
            .fold(None, |bounds: Option<Self>, point| {
                let at = Self::from_origin(point, 0.0, 0.0);
                Some(bounds.map_or(at, |bounds| bounds.union(at)))
            })
    }

    /// The horizontal extent.
    #[must_use]
    pub(crate) fn width(self) -> f32 {
        self.right - self.left
    }

    /// The vertical extent.
    #[must_use]
    pub(crate) fn height(self) -> f32 {
        self.bottom - self.top
    }

    /// The middle of the rectangle.
    #[must_use]
    pub(crate) fn center(self) -> Point {
        Point::new(
            self.left.midpoint(self.right),
            self.top.midpoint(self.bottom),
        )
    }

    /// The smallest rectangle holding both.
    #[must_use]
    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    /// The rectangle grown by `margin` on every side, kept within `bounds`.
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
}
