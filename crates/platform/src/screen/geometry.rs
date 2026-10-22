//! Display rectangles, frame sizes, and pixel rows: the arithmetic native adapters share, free of
//! native calls so every host tests it.

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
    /// Where a point lies within these bounds as a share of each axis: `0.0..=1.0` spans them, and
    /// a point beyond an edge falls outside that range. `None` when the bounds are empty.
    pub(crate) fn normalize(self, [x, y]: [f64; 2]) -> Option<[f32; 2]> {
        let [left, top] = self.origin;
        let [width, height] = self.size;
        let share = |offset: f64, extent: f64| (extent > 0.0).then(|| narrow(offset / extent));
        Some([share(x - left, width)?, share(y - top, height)?])
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
            assert_eq!(pixels([1440.0, 900.0], scale), None, "scale {scale}");
        }
        assert_eq!(pixels([0.2, 900.0], 2.0), None);
        assert_eq!(pixels([1440.0, 900.0], 1e10), None);
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
            1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, //
            9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0,
        ];
        assert_eq!(
            pack_rows(&rows, 12, 2, 2),
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
        assert_eq!(pack_rows(&rows, 8, 2, 5), None, "a short buffer");
        assert_eq!(pack_rows(&rows, 4, 2, 4), None, "overlapping rows");
        assert_eq!(pack_rows(&rows, 0, 1, 1), None, "no stride");
        assert_eq!(pack_rows(&rows, 8, 0, 4), None, "no width");
    }
}
