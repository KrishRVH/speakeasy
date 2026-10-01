//! Sixteen-pixel line glyphs stroked on a canvas.

use gpui::{PathBuilder, Pixels, Point, Window, canvas, point, prelude::*, px, rgb};

#[derive(Clone, Copy)]
pub(super) enum Symbol {
    Microphone,
    Lock,
    Check,
    Attention,
}

impl Symbol {
    fn strokes(self) -> &'static [&'static [(f32, f32)]] {
        match self {
            Self::Microphone => &[
                &[(6.0, 2.0), (10.0, 2.0), (10.0, 8.0), (6.0, 8.0), (6.0, 2.0)],
                &[(3.0, 6.0), (3.0, 11.0), (13.0, 11.0), (13.0, 6.0)],
                &[(8.0, 11.0), (8.0, 14.0)],
            ],
            Self::Lock => &[
                &[(5.0, 7.0), (5.0, 3.0), (11.0, 3.0), (11.0, 7.0)],
                &[
                    (3.0, 7.0),
                    (13.0, 7.0),
                    (13.0, 14.0),
                    (3.0, 14.0),
                    (3.0, 7.0),
                ],
            ],
            Self::Check => &[&[(2.0, 8.0), (6.0, 12.0), (14.0, 4.0)]],
            Self::Attention => &[&[(8.0, 2.0), (8.0, 10.0)], &[(8.0, 12.0), (8.0, 14.0)]],
        }
    }

    pub(super) fn element(self, color: u32) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| self.paint(bounds.origin, color, window),
        )
        .size(px(16.0))
        .flex_shrink_0()
    }

    fn paint(self, origin: Point<Pixels>, color: u32, window: &mut Window) {
        let vertex = |&(x, y): &(f32, f32)| point(origin.x + px(x), origin.y + px(y));
        let mut path = PathBuilder::stroke(px(1.5));
        for stroke in self.strokes() {
            let Some((first, rest)) = stroke.split_first() else {
                continue;
            };
            path.move_to(vertex(first));
            for next in rest {
                path.line_to(vertex(next));
            }
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, rgb(color));
        }
    }
}
