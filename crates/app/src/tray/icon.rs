use crate::{
    status::Indicator,
    theme::{Theme, quantize},
};

pub(super) const SIZE: u32 = 32;
const BYTES: usize = (SIZE * SIZE * 4) as usize;

const BARS: [(f32, f32); 5] = [
    (8.0, 4.0),
    (11.5, 6.0),
    (15.0, 8.0),
    (18.5, 6.0),
    (22.0, 4.0),
];

const LIGHT_BADGE: u32 = 0xE0_E0_E6;
const BADGE_CENTER: f32 = 25.0;

const RING_RADIUS: f32 = 4.2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum IconStyle {
    Color,
    Template,
}

struct Mark {
    style: IconStyle,
    badge: Option<Badge>,
    slot_color: u32,
    slot_opacity: f32,
    badge_color: u32,
}

impl Mark {
    fn new(indicator: Indicator, theme: Theme, style: IconStyle) -> Self {
        let palette = theme.palette();
        Self {
            style,
            badge: match indicator {
                Indicator::Ready => None,
                Indicator::Recording => (style == IconStyle::Template).then_some(Badge::Recording),
                Indicator::Busy => Some(Badge::Busy),
                Indicator::Paused => Some(Badge::Paused),
                Indicator::Attention => Some(Badge::Attention),
            },
            slot_color: if indicator == Indicator::Recording && style == IconStyle::Color {
                palette.live
            } else {
                palette.lamp
            },
            slot_opacity: if indicator == Indicator::Paused {
                0.5
            } else {
                1.0
            },
            badge_color: match indicator {
                Indicator::Recording => palette.live,
                Indicator::Attention => palette.warn,
                Indicator::Ready | Indicator::Busy | Indicator::Paused => LIGHT_BADGE,
            },
        }
    }

    fn pixel(&self, x: f32, y: f32) -> [u8; 4] {
        let slot = rounded_rect(x, y, [3.0, 9.0, 29.0, 23.0], 7.0);
        let bars = BARS
            .into_iter()
            .map(|(left, height)| {
                let bar = [left, 16.0 - height / 2.0, left + 2.0, 16.0 + height / 2.0];
                coverage(rounded_rect(x, y, bar, 1.0))
            })
            .fold(0.0, f32::max);
        let (badge, cut) = match self.badge {
            Some(badge) => (badge.distance(x, y), coverage(badge_diamond(x, y, 7.8))),
            None => (f32::MAX, 0.0),
        };

        let keyline = match self.style {
            IconStyle::Color => coverage(slot.min(badge) - 1.0) * 0.45,
            IconStyle::Template => 0.0,
        };
        let (color, opacity) = composite([
            (0x00_00_00, keyline),
            (
                self.slot_color,
                coverage(slot) * (1.0 - bars) * (1.0 - cut) * self.slot_opacity,
            ),
            (self.badge_color, coverage(badge)),
        ]);
        if opacity > 0.0 {
            let [red, green, blue] = match self.style {
                IconStyle::Color => color,
                IconStyle::Template => [0.0; 3],
            };
            [red, green, blue, opacity * 255.0].map(quantize)
        } else {
            [0; 4]
        }
    }
}

#[derive(Clone, Copy)]
enum Badge {
    Recording,
    Busy,
    Paused,
    Attention,
}

impl Badge {
    fn distance(self, x: f32, y: f32) -> f32 {
        match self {
            Self::Recording => badge_diamond(x, y, 5.0),
            Self::Busy => {
                let (dx, dy) = (x - BADGE_CENTER, y - BADGE_CENTER);
                let from_centerline = if dx < 0.0 && dy < 0.0 {
                    dx.hypot(dy + RING_RADIUS).min((dx + RING_RADIUS).hypot(dy))
                } else {
                    (dx.hypot(dy) - RING_RADIUS).abs()
                };
                from_centerline - 0.95
            },
            Self::Paused => {
                let left = rounded_rect(x, y, [21.9, 20.8, 24.2, 29.2], 0.6);
                let right = rounded_rect(x, y, [25.8, 20.8, 28.1, 29.2], 0.6);
                left.min(right)
            },
            Self::Attention => {
                let stroke = rounded_rect(x, y, [23.85, 19.6, 26.15, 26.0], 1.15);
                let dot = (x - BADGE_CENTER).hypot(y - 28.7) - 1.3;
                stroke.min(dot)
            },
        }
    }
}

pub(super) fn raster(indicator: Indicator, theme: Theme, style: IconStyle) -> Vec<u8> {
    let mark = Mark::new(indicator, theme, style);
    let mut rgba = Vec::with_capacity(BYTES);
    for row in 0..SIZE {
        for column in 0..SIZE {
            rgba.extend(mark.pixel(column as f32 + 0.5, row as f32 + 0.5));
        }
    }
    rgba
}

fn rounded_rect(x: f32, y: f32, [left, top, right, bottom]: [f32; 4], radius: f32) -> f32 {
    let qx = (x - f32::midpoint(left, right)).abs() - ((right - left) / 2.0 - radius);
    let qy = (y - f32::midpoint(top, bottom)).abs() - ((bottom - top) / 2.0 - radius);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

fn badge_diamond(x: f32, y: f32, size: f32) -> f32 {
    ((x - BADGE_CENTER).abs() + (y - BADGE_CENTER).abs() - size) * std::f32::consts::FRAC_1_SQRT_2
}

fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

fn composite(layers: [(u32, f32); 3]) -> ([f32; 3], f32) {
    layers
        .into_iter()
        .fold(([0.0; 3], 0.0), |(mut rgb, alpha), (color, opacity)| {
            let combined = opacity + alpha * (1.0 - opacity);
            if combined <= 0.0 {
                return (rgb, 0.0);
            }
            let [_, red, green, blue] = color.to_be_bytes();
            for (under, over) in rgb.iter_mut().zip([red, green, blue]) {
                *under = (f32::from(over) * opacity + *under * alpha * (1.0 - opacity)) / combined;
            }
            (rgb, combined)
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn template_states_differ_by_shape_and_recording_changes_color() {
        let states = [
            Indicator::Ready,
            Indicator::Recording,
            Indicator::Busy,
            Indicator::Paused,
            Indicator::Attention,
        ];
        let shapes: HashSet<Vec<u8>> = states
            .into_iter()
            .map(|state| {
                raster(state, Theme::Jet, IconStyle::Template)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&[.., alpha]| alpha)
                    .collect()
            })
            .collect();
        assert_eq!(
            shapes.len(),
            states.len(),
            "Two template states share a shape"
        );
        assert_ne!(
            raster(Indicator::Ready, Theme::Jet, IconStyle::Color),
            raster(Indicator::Recording, Theme::Jet, IconStyle::Color)
        );
    }
}
