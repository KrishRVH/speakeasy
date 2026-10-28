//! Display rectangles, frame sizes, and pixel rows: the arithmetic native adapters share, free of
//! native calls so every host tests it.

use super::Part;

/// A rectangle in the platform's global display space: points on macOS, physical pixels on
/// Windows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bounds {
    /// The corner with the smallest coordinates.
    pub(crate) origin: [f64; 2],
    /// Width and height.
    pub(crate) size: [f64; 2],
}

impl Bounds {
    /// Where a point lies within these bounds, normalized on each axis: `0.0..=1.0` spans them, and
    /// a point beyond an edge falls outside that range. `None` when the bounds are empty.
    pub(crate) fn normalize(self, [x, y]: [f64; 2]) -> Option<[f32; 2]> {
        let [left, top] = self.origin;
        let [width, height] = self.size;
        let normalized = |offset: f64, extent: f64| (extent > 0.0).then(|| narrow(offset / extent));
        Some([normalized(x - left, width)?, normalized(y - top, height)?])
    }

    /// Whether `point` lies within these bounds, counting the edges through `origin` but not the
    /// opposite ones, so a point on an edge two adjoining bounds share lies in exactly one of them.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn contains(self, [x, y]: [f64; 2]) -> bool {
        let [left, top] = self.origin;
        let [width, height] = self.size;
        (left..left + width).contains(&x) && (top..top + height).contains(&y)
    }

    /// The part of these bounds that `other` covers, clipped to them; `None` when the two do not
    /// overlap or these bounds are empty.
    pub(crate) fn part_covered_by(self, other: Self) -> Option<Part> {
        let [left, top] = other.origin;
        let [width, height] = other.size;
        let part = Part::between(
            self.normalize([left, top])?,
            self.normalize([left + width, top + height])?,
        );
        (part.width() > 0.0 && part.height() > 0.0).then_some(part)
    }

    /// The same rectangle measured from the opposite edge of a space `extent` tall, turning
    /// top-left coordinates into bottom-left ones and back.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn flipped(self, extent: f64) -> Self {
        let [x, y] = self.origin;
        let [_, height] = self.size;
        Self {
            origin: [x, extent - y - height],
            size: self.size,
        }
    }
}

/// `points` at `scale` pixels per point, rounded to whole pixels; `None` unless each axis comes to
/// at least one pixel within `u32` range.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn pixels(points: [f64; 2], scale: f64) -> Option<[u32; 2]> {
    let [width, height] = points.map(|extent| whole(extent * scale));
    Some([width?, height?])
}

/// The pixels of a `[width, height]` image that `part` covers, as its left, top, right, and
/// bottom edges, rounded to whole pixels: at least one each way, unless the image is empty, and
/// none outside it.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn pixel_box(part: Part, [width, height]: [u32; 2]) -> [u32; 4] {
    let (left, right) = pixel_span(part.left, part.right, width);
    let (top, bottom) = pixel_span(part.top, part.bottom, height);
    [left, top, right, bottom]
}

/// The pixels of an `extent`-pixel axis between the normalized edges `start` and `end`.
#[cfg(any(target_os = "windows", test))]
fn pixel_span(start: f32, end: f32, extent: u32) -> (u32, u32) {
    let edge = |normalized: f32| whole_within(f64::from(normalized) * f64::from(extent), extent);
    let first = edge(start).min(extent.saturating_sub(1));
    (first, edge(end).max(first.saturating_add(1)).min(extent))
}

/// The part of a `[width, height]` image that `[left, top, right, bottom]`, a box of its pixels,
/// shows.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn part_of_box([left, top, right, bottom]: [u32; 4], [width, height]: [u32; 2]) -> Part {
    let normalized = |pixels: u32, extent: u32| {
        if extent == 0 {
            0.0
        } else {
            narrow(f64::from(pixels) / f64::from(extent))
        }
    };
    Part {
        left: normalized(left, width),
        top: normalized(top, height),
        right: normalized(right, width),
        bottom: normalized(bottom, height),
    }
}

/// `size` halved until its long edge fits `longest_edge`, stopping before an axis would vanish.
pub(crate) fn fit(size: [u32; 2], longest_edge: u32) -> [u32; 2] {
    let [mut width, mut height] = size;
    while width.max(height) > longest_edge && width.min(height) > 1 {
        width /= 2;
        height /= 2;
    }
    [width, height]
}

/// Copies `height` rows of `width` four-byte pixels out of `rows`, where each row starts `stride`
/// bytes after the last, into one buffer without padding.
///
/// `None` when the image is empty, rows would overlap, or `rows` ends early.
pub(crate) fn pack_rows(
    rows: &[u8],
    stride: usize,
    width: usize,
    height: usize,
) -> Option<Vec<u8>> {
    let row = width.checked_mul(4)?;
    let size = row.checked_mul(height)?;
    if size == 0 || stride < row {
        return None;
    }
    let mut packed = Vec::with_capacity(size);
    for line in rows.chunks(stride).take(height) {
        packed.extend_from_slice(line.get(..row)?);
    }
    (packed.len() == size).then_some(packed)
}

/// Narrows a display coordinate to the `f32` the portable types carry.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Display coordinates need far less precision than f32 holds"
)]
pub(crate) fn narrow(value: f64) -> f32 {
    value as f32
}

/// Rounds a pixel measure to a count of at least one.
#[cfg(any(target_os = "macos", test))]
#[expect(
    clippy::cast_possible_truncation,
    reason = "The rounded value is checked to lie within u32 range first"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "The value is checked to be positive first"
)]
fn whole(value: f64) -> Option<u32> {
    let value = value.round();
    (1.0..=f64::from(u32::MAX))
        .contains(&value)
        .then_some(value as u32)
}

/// Rounds a pixel measure to the nearest count from zero to `most`.
#[cfg(any(target_os = "windows", test))]
#[expect(
    clippy::cast_possible_truncation,
    reason = "The rounded value is clamped to lie within u32 range first"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "The value is clamped to be nonnegative first, and NaN casts to zero"
)]
fn whole_within(value: f64, most: u32) -> u32 {
    value.round().clamp(0.0, f64::from(most)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1440 × 900 main display at the origin of Core Graphics' global space.
    const MAIN: Bounds = Bounds {
        origin: [0.0, 0.0],
        size: [1440.0, 900.0],
    };

    #[test]
    fn pointer_normalizes_against_a_display_left_of_and_above_the_main_one() {
        let display = Bounds {
            origin: [-1920.0, -300.0],
            size: [1920.0, 1080.0],
        };
        assert_eq!(display.normalize([-1920.0, -300.0]), Some([0.0, 0.0]));
        assert_eq!(display.normalize([0.0, 780.0]), Some([1.0, 1.0]));
        assert_eq!(display.normalize([-960.0, 240.0]), Some([0.5, 0.5]));
    }

    #[test]
    fn pointer_off_the_display_falls_outside_the_unit_range() {
        assert_eq!(MAIN.normalize([2160.0, -450.0]), Some([1.5, -0.5]));
    }

    #[test]
    fn empty_bounds_normalize_nothing() {
        let gone = Bounds {
            origin: [0.0, 0.0],
            size: [0.0, 0.0],
        };
        assert_eq!(gone.normalize([0.0, 0.0]), None);
    }

    #[test]
    fn flipping_measures_each_display_from_the_bottom_of_the_main_one() {
        assert_eq!(MAIN.flipped(900.0), MAIN);
        let above = Bounds {
            origin: [200.0, -1080.0],
            size: [1920.0, 1080.0],
        };
        assert_eq!(above.flipped(900.0).origin, [200.0, 900.0]);
        let below_right = Bounds {
            origin: [1440.0, 600.0],
            size: [1280.0, 800.0],
        };
        assert_eq!(below_right.flipped(900.0).origin, [1440.0, -500.0]);
        assert_eq!(below_right.flipped(900.0).flipped(900.0), below_right);
    }

    #[test]
    fn points_scale_to_whole_pixels() {
        assert_eq!(pixels([1470.0, 956.0], 2.0), Some([2940, 1912]));
        assert_eq!(pixels([1512.0, 982.5], 1.0), Some([1512, 983]));
    }

    #[test]
    fn degenerate_scales_have_no_pixels() {
        for scale in [0.0, -2.0, f64::NAN, f64::INFINITY] {
            assert_eq!(pixels([1440.0, 900.0], scale), None, "Scale {scale}");
        }
        assert_eq!(pixels([0.2, 900.0], 2.0), None);
        assert_eq!(pixels([1440.0, 900.0], 1e10), None);
    }

    #[test]
    fn a_window_covers_the_part_of_its_display_it_overlaps() {
        let window = |origin: [f64; 2], size: [f64; 2]| Bounds { origin, size };
        assert_eq!(
            MAIN.part_covered_by(window([360.0, 225.0], [720.0, 450.0])),
            Some(Part::between([0.25, 0.25], [0.75, 0.75]))
        );
        assert_eq!(
            MAIN.part_covered_by(window([-720.0, 450.0], [1440.0, 900.0])),
            Some(Part::between([0.0, 0.5], [0.5, 1.0])),
            "Clipped to the display"
        );
        assert_eq!(
            MAIN.part_covered_by(window([1440.0, 0.0], [800.0, 600.0])),
            None,
            "On the next display"
        );
    }

    #[test]
    fn bounds_hold_their_top_left_edges_only() {
        assert!(MAIN.contains([0.0, 0.0]));
        assert!(MAIN.contains([1439.5, 899.5]));
        assert!(
            !MAIN.contains([1440.0, 450.0]),
            "The next display's left edge"
        );
        assert!(!MAIN.contains([-0.5, 450.0]));
    }

    #[test]
    fn a_part_reads_back_whole_pixels_inside_the_image() {
        let size = [1920, 1080];
        assert_eq!(pixel_box(Part::WHOLE, size), [0, 0, 1920, 1080]);
        let middle = Part::between([0.25, 0.5], [0.75, 0.75]);
        assert_eq!(pixel_box(middle, size), [480, 540, 1440, 810]);
        assert_eq!(part_of_box(pixel_box(middle, size), size), middle);
        let sliver = Part::between([1.0, 0.0], [1.0, 1.0]);
        assert_eq!(
            pixel_box(sliver, size),
            [1919, 0, 1920, 1080],
            "One pixel wide"
        );
        assert_eq!(pixel_box(middle, [0, 0]), [0, 0, 0, 0]);
    }

    #[test]
    fn frames_halve_until_the_long_edge_fits() {
        assert_eq!(fit([2940, 1912], 1568), [1470, 956]);
        assert_eq!(fit([2940, 1912], 1000), [735, 478]);
        assert_eq!(fit([1440, 900], 1440), [1440, 900]);
        assert_eq!(fit([1080, 1920], 1000), [540, 960]);
    }

    #[test]
    fn halving_never_empties_an_axis() {
        assert_eq!(fit([4096, 3], 16), [2048, 1]);
        assert_eq!(fit([8, 8], 0), [1, 1]);
    }

    #[test]
    fn padded_rows_pack_tightly() {
        let rows = [
            [1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0],
            [9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0],
        ];
        assert_eq!(
            pack_rows(rows.as_flattened(), 12, 2, 2),
            Some((1..=16).collect::<Vec<u8>>())
        );
    }

    #[test]
    fn the_last_row_needs_no_padding() {
        let rows = [1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8];
        assert_eq!(
            pack_rows(&rows, 8, 1, 2),
            Some(vec![1, 2, 3, 4, 5, 6, 7, 8])
        );
    }

    #[test]
    fn rows_that_cannot_hold_the_image_pack_nothing() {
        let rows = [0; 32];
        assert_eq!(pack_rows(&rows, 8, 2, 5), None, "A short buffer");
        assert_eq!(pack_rows(&rows, 4, 2, 4), None, "Overlapping rows");
        assert_eq!(pack_rows(&rows, 0, 1, 1), None, "No stride");
        assert_eq!(pack_rows(&rows, 8, 0, 4), None, "No width");
    }
}
