//! The menu bar mark: a door slot with stepped bars cut through it, as a template image that macOS
//! recolors. States differ by shape alone: Recording, Busy, Paused, and Attention each add a
//! cut-jewel badge, and Paused dims the slot. Shapes are signed distances in icon pixels, negative
//! inside.

use speakeasy_dictation::{status::Indicator, theme::quantize};

pub(super) const SIZE: u32 = 32;
const BYTES: usize = (SIZE * SIZE * 4) as usize;
/// Two-pixel bars cut through the slot, as (left edge, height), centered on y = 16.
const BARS: [(f32, f32); 5] = [
    (8.0, 4.0),
    (11.5, 6.0),
    (15.0, 8.0),
    (18.5, 6.0),
    (22.0, 4.0),
];
const BADGE_CENTER: f32 = 25.0;
/// The Busy badge's ring, measured to the middle of its stroke.
const RING_RADIUS: f32 = 4.2;

/// Black RGBA pixels with straight alpha, `SIZE` by `SIZE`.
pub(super) fn raster(indicator: Indicator) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(BYTES);
    for row in 0..SIZE {
        for column in 0..SIZE {
            let opacity = opacity(indicator, column as f32 + 0.5, row as f32 + 0.5);
            rgba.extend([0, 0, 0, quantize(opacity * 255.0)]);
        }
    }
    rgba
}

/// The mark's opacity at a pixel center: the badge composited over the slot it cuts.
fn opacity(indicator: Indicator, x: f32, y: f32) -> f32 {
    let bars = BARS.iter().fold(0.0_f32, |cover, &(left, height)| {
        let bar = [left, 16.0 - height / 2.0, left + 2.0, 16.0 + height / 2.0];
        cover.max(coverage(rounded_rect(x, y, bar, 1.0)))
    });
    let (badge, cut) = badge(indicator, x, y).map_or((0.0, 0.0), |badge| {
        (coverage(badge), coverage(badge_diamond(x, y, 7.8)))
    });
    let dim = if indicator == Indicator::Paused {
        0.5
    } else {
        1.0
    };
    let slot = coverage(rounded_rect(x, y, [3.0, 9.0, 29.0, 23.0], 7.0));
    (slot * (1.0 - bars) * (1.0 - cut) * dim).mul_add(1.0 - badge, badge)
}

/// The signed distance to the state's badge, if it has one.
fn badge(indicator: Indicator, x: f32, y: f32) -> Option<f32> {
    Some(match indicator {
        Indicator::Ready => return None,
        Indicator::Recording => badge_diamond(x, y, 5.0),
        Indicator::Busy => {
            // A three-quarter ring with round caps, open at the upper left.
            let (dx, dy) = (x - BADGE_CENTER, y - BADGE_CENTER);
            let from_centerline = if dx < 0.0 && dy < 0.0 {
                dx.hypot(dy + RING_RADIUS).min((dx + RING_RADIUS).hypot(dy))
            } else {
                (dx.hypot(dy) - RING_RADIUS).abs()
            };
            from_centerline - 0.95
        },
        Indicator::Paused => {
            let left = rounded_rect(x, y, [21.9, 20.8, 24.2, 29.2], 0.6);
            let right = rounded_rect(x, y, [25.8, 20.8, 28.1, 29.2], 0.6);
            left.min(right)
        },
        Indicator::Attention => {
            let stroke = rounded_rect(x, y, [23.85, 19.6, 26.15, 26.0], 1.15);
            let dot = (x - BADGE_CENTER).hypot(y - 28.7) - 1.3;
            stroke.min(dot)
        },
    })
}

fn rounded_rect(x: f32, y: f32, [left, top, right, bottom]: [f32; 4], radius: f32) -> f32 {
    let qx = (x - f32::midpoint(left, right)).abs() - ((right - left) / 2.0 - radius);
    let qy = (y - f32::midpoint(top, bottom)).abs() - ((bottom - top) / 2.0 - radius);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

/// A diamond centered on the badge, its corners `size` pixels from the center.
fn badge_diamond(x: f32, y: f32, size: f32) -> f32 {
    ((x - BADGE_CENTER).abs() + (y - BADGE_CENTER).abs() - size) * std::f32::consts::FRAC_1_SQRT_2
}

fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_its_own_shape() {
        let states = [
            Indicator::Ready,
            Indicator::Recording,
            Indicator::Busy,
            Indicator::Paused,
            Indicator::Attention,
        ];
        let shapes: Vec<Vec<u8>> = states
            .iter()
            .map(|&state| {
                raster(state)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[3])
                    .collect()
            })
            .collect();
        for (index, shape) in shapes.iter().enumerate() {
            assert!(shapes[index + 1..].iter().all(|other| other != shape));
        }
    }
}
