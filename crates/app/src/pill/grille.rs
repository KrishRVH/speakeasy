//! The door slot: a mirrored grille of recent levels behind a sliding lid. Everything paints inside
//! one canvas, so bar geometry never touches layout or hit testing as the level moves.

use gpui::{
    Bounds, BoxShadow, ContentMask, Corners, PathBuilder, Pixels, Rgba, Window, fill,
    linear_color_stop, linear_gradient, point, px, rgb, size,
};

use super::METER_BARS;
use crate::theme::{Palette, alpha, mix};

const OLDEST_AGE: usize = METER_BARS - 1;
const BAR_WIDTH: f32 = 2.0;
/// The distance from one grille bar's left edge to the next.
const BAR_PITCH: f32 = 4.0;

#[derive(Clone, Copy)]
pub(super) struct Grille {
    /// How far the lid covers the slot, from 0 (open) to 1 (closed).
    pub(super) lid: f32,
    /// The waveform's opacity, from 0 (dark) to 1 (lit).
    pub(super) waveform: f32,
    /// Recent levels, oldest first.
    pub(super) levels: [f32; METER_BARS],
    pub(super) reduced_motion: bool,
    /// The processing glow's position across the open slot, from 0 (left) to 1 (right).
    pub(super) sweep: Option<f32>,
    pub(super) palette: &'static Palette,
}

impl Grille {
    pub(super) fn shows_waveform(&self) -> bool {
        self.waveform > 0.01
    }

    pub(super) fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let slot = Slot::new(bounds);
        let open = slot.width * (1.0 - self.lid);
        if self.shows_waveform() {
            self.paint_backlight(slot, window);
            let mask = ContentMask {
                bounds: rect(slot.x, slot.y, open, slot.height),
            };
            window.with_content_mask(Some(mask), |window| {
                if self.reduced_motion {
                    self.paint_level(slot, window);
                } else {
                    self.paint_mirrored_levels(slot, window);
                }
            });
        }
        if let Some(position) = self.sweep
            && open > 16.0
        {
            self.paint_sweep(slot, open, position, window);
        }
        self.paint_lid(slot, open, window);
    }

    fn paint_backlight(&self, slot: Slot, window: &mut Window) {
        let glow = self.level_at_age(0).mul_add(0.22, 0.1) * self.waveform;
        window.paint_quad(
            fill(slot.bounds(), alpha(self.palette.live, glow)).corner_radii(px(slot.radius())),
        );
    }

    fn paint_level(&self, slot: Slot, window: &mut Window) {
        let bar = slot.bar_height(self.level_at_age(0));
        let top = slot.y + (slot.height - bar) / 2.0;
        window.paint_quad(
            fill(
                rect(slot.x + 9.0, top, slot.width - 18.0, bar),
                self.bar_color(),
            )
            .corner_radii(px(bar.min(6.0) / 2.0)),
        );
    }

    fn paint_mirrored_levels(&self, slot: Slot, window: &mut Window) {
        let Slot {
            x,
            y,
            width,
            height,
        } = slot;
        let radius = slot.radius();
        let color = self.bar_color();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "The bounded slot width intentionally rounds down to a whole number of bars"
        )]
        #[expect(
            clippy::cast_sign_loss,
            reason = "The bar count is clamped to at least one before converting from layout coordinates"
        )]
        let count = ((width - 12.0) / BAR_PITCH).floor().max(1.0) as usize;
        let span = (count as f32 - 1.0).mul_add(BAR_PITCH, BAR_WIDTH);
        let start = x + (width - span) / 2.0;
        for index in 0..count {
            let left = (index as f32).mul_add(BAR_PITCH, start);
            let center = left + BAR_WIDTH / 2.0;
            // A bar over a rounded end stays within the end's chord through the bar's center.
            let into_end = (x + radius - center)
                .max(center - (x + width - radius))
                .max(0.0);
            let bar = slot
                .bar_height(self.level_at_age(mirrored_age(index, count)))
                .min((chord(radius, into_end) - 3.0).max(2.0));
            let bounds = rect(left, y + (height - bar) / 2.0, BAR_WIDTH, bar);
            window.paint_quad(fill(bounds, color).corner_radii(px(BAR_WIDTH / 2.0)));
        }
    }

    fn paint_sweep(&self, slot: Slot, open: f32, position: f32, window: &mut Window) {
        let glow = rect(
            position.mul_add(open - 18.0, slot.x + 4.0),
            slot.y + 3.0,
            10.0,
            slot.height - 6.0,
        );
        let corners = Corners::all(px(((slot.height - 6.0) / 2.0).min(5.0)));
        window.paint_shadows(
            glow,
            corners,
            &[BoxShadow {
                color: alpha(self.palette.lamp, 0.6).into(),
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(6.0),
                spread_radius: px(0.0),
            }],
        );
        window.paint_quad(fill(glow, alpha(self.palette.lamp, 0.85)).corner_radii(corners));
    }

    fn paint_lid(&self, slot: Slot, open: f32, window: &mut Window) {
        let cover = slot.width - open;
        if cover < 1.0 {
            return;
        }
        let palette = self.palette;
        let radius = slot.radius();
        let left = slot.x + open;
        let height = if cover < radius {
            chord(radius, radius - cover)
        } else {
            slot.height
        };
        let top = slot.y + (slot.height - height) / 2.0;
        let leading = px((radius - open).max(0.0));
        let trailing = px((height / 2.0).min(cover));
        window.paint_quad(
            fill(
                rect(left, top, cover, height),
                linear_gradient(
                    180.0,
                    linear_color_stop(rgb(mix(palette.raise, palette.ink, 0.06)), 0.0),
                    linear_color_stop(rgb(palette.door), 1.0),
                ),
            )
            .corner_radii(Corners {
                top_left: leading,
                top_right: trailing,
                bottom_right: trailing,
                bottom_left: leading,
            }),
        );
        if open >= 0.5 {
            window.paint_quad(fill(rect(left, top, 1.0, height), alpha(palette.ink, 0.2)));
        }
        if cover > 20.0 && slot.height > 12.0 {
            keystone(
                left + 7.0,
                slot.y + radius,
                alpha(palette.lamp, 0.8),
                window,
            );
        }
    }

    fn bar_color(&self) -> Rgba {
        alpha(self.palette.live, self.waveform)
    }

    fn level_at_age(&self, age: usize) -> f32 {
        self.levels.iter().rev().nth(age).copied().unwrap_or(0.0)
    }
}

#[derive(Clone, Copy)]
struct Slot {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Slot {
    fn new(bounds: Bounds<Pixels>) -> Self {
        Self {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
        }
    }

    fn bounds(self) -> Bounds<Pixels> {
        rect(self.x, self.y, self.width, self.height)
    }

    fn radius(self) -> f32 {
        self.height / 2.0
    }

    /// A level bar's height: two pixels when silent, two short of each edge at full level.
    fn bar_height(self, level: f32) -> f32 {
        level.mul_add(self.height - 6.0, 2.0)
    }
}

fn rect(left: f32, top: f32, width: f32, height: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(left), px(top)), size(px(width), px(height)))
}

/// The age of the level bar `index` shows: the center bar, or both center bars of an even count,
/// show the newest level, and ages grow symmetrically outward up to the oldest level.
fn mirrored_age(index: usize, count: usize) -> usize {
    (index.abs_diff(count.saturating_sub(1).saturating_sub(index)) / 2).min(OLDEST_AGE)
}

/// The length of a chord `offset` away from the center of a circle.
fn chord(radius: f32, offset: f32) -> f32 {
    2.0 * offset.mul_add(-offset, radius * radius).max(0.0).sqrt()
}

fn keystone(x: f32, y: f32, color: Rgba, window: &mut Window) {
    const RADIUS: f32 = 2.4;
    let mut path = PathBuilder::fill();
    path.move_to(point(px(x), px(y - RADIUS)));
    path.line_to(point(px(x + RADIUS), px(y)));
    path.line_to(point(px(x), px(y + RADIUS)));
    path.line_to(point(px(x - RADIUS), px(y)));
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_bars_show_the_newest_level_and_ages_grow_outward_to_the_oldest() {
        let ages = |count| {
            (0..count)
                .map(|index| mirrored_age(index, count))
                .collect::<Vec<_>>()
        };
        assert_eq!(ages(5), [2, 1, 0, 1, 2]);
        assert_eq!(ages(4), [1, 0, 0, 1]);
        assert_eq!(ages(1), [0]);
        for count in 1..=80 {
            assert!(
                ages(count).contains(&0),
                "{count} bars leave the newest level undrawn"
            );
        }
        assert!(ages(80).iter().all(|&age| age <= OLDEST_AGE));
        assert_eq!(ages(80).first(), Some(&OLDEST_AGE));
    }
}
