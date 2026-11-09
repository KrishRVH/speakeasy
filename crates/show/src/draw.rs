pub(crate) mod video;

use std::time::Duration;

use tiny_skia::{FillRule, Paint, Path, PathBuilder, PixmapMut, Transform};

use crate::{
    Image, Keyframe, Mark, Moment, Point, Rect, Shape, Span, Typeface, ink,
    marks::MARK_GAP,
    recording::{Action, Recording},
};

const WHITE: [u8; 4] = [255, 255, 255, 255];

const RECENT_INK: Duration = Duration::from_millis(1_200);

const _: () = assert!(
    RECENT_INK.as_nanos() >= MARK_GAP.as_nanos(),
    "recent ink spans at least the pause that joins strokes into one mark"
);

const RIPPLE_REACH: Duration = Duration::from_millis(600);

const CLICK_RING: f32 = 10.0;
const CLICK_LINE: f32 = 1.6;
const CLICK_DOT: f32 = 1.6;
const CLICK_EDGE: f32 = 1.4;
const CLICK_BLUE: [u8; 4] = [0, 96, 214, 230];
const CLICK_WHITE: [u8; 4] = [255, 255, 255, 230];

const BADGE_BAND: f32 = 0.18;
const BADGE_SIDE: f32 = 0.30;

const QUARTER_CIRCLE_HANDLE: f32 = 0.552_284_8;

#[derive(Clone, Copy)]
pub(crate) enum Corner {
    Left,
    Right,
}

pub(crate) struct Badge {
    pub number: usize,
    pub time: String,
}

pub(crate) struct Shown<'a> {
    pub ink: Vec<Vec<Point>>,
    pub focus: Option<Point>,
    pub labels: Vec<&'a Mark>,
}

impl<'a> Shown<'a> {
    pub(crate) fn on(keyframe: &Keyframe, recording: &Recording, marks: &'a [Mark]) -> Self {
        let own = keyframe.marks_in(marks);
        let owned = |index: usize| own.iter().any(|mark| mark.strokes.contains(&index));
        let recent =
            |span: Span| span.start() <= keyframe.at && keyframe.at.since(span.end()) <= RECENT_INK;
        let ink = recording
            .strokes
            .iter()
            .enumerate()
            .filter_map(|(index, stroke)| {
                if owned(index) {
                    Some(stroke.path().collect())
                } else if own.is_empty() && recent(stroke.span()) {
                    Some(stroke.path_until(keyframe.at).collect())
                } else {
                    None
                }
            })
            .collect();
        let focus = keyframe
            .action
            .map(Action::point)
            .or_else(|| nearest_click(&recording.actions, keyframe.at))
            .filter(|point| point.is_finite());
        let recent_marks = marks
            .iter()
            .filter(|mark| own.is_empty() && recent(mark.span));
        Self {
            ink,
            focus,
            labels: own.iter().chain(recent_marks).collect(),
        }
    }

    pub(crate) fn points(&self) -> impl Iterator<Item = Point> {
        let anchors = self.labels.iter().flat_map(|mark| match mark.shape {
            Shape::Pin { point } => vec![point],
            _ => vec![
                Point::new(mark.bounds.left, mark.bounds.top),
                Point::new(mark.bounds.right, mark.bounds.bottom),
            ],
        });
        self.ink
            .iter()
            .flatten()
            .copied()
            .chain(self.focus)
            .chain(anchors)
    }
}

pub(crate) struct Drawn {
    occupied: Vec<Point>,
    focus: Option<Point>,
    extent: Option<Rect>,
}

impl Drawn {
    pub(crate) const fn extent(&self) -> Option<Rect> {
        self.extent
    }

    pub(crate) fn corner(&self, view: Rect) -> Corner {
        let within = |point: &Point| {
            Point::new(
                (point.x - view.left) / view.width(),
                (point.y - view.top) / view.height(),
            )
        };
        let in_top_left = |point: Point| point.x < BADGE_SIDE && point.y < BADGE_BAND;
        let in_top_right = |point: Point| point.x > 1.0 - BADGE_SIDE && point.y < BADGE_BAND;
        let crowded_left = self
            .focus
            .iter()
            .chain(&self.occupied)
            .map(within)
            .any(in_top_left);
        if crowded_left && !self.focus.iter().map(within).any(in_top_right) {
            Corner::Right
        } else {
            Corner::Left
        }
    }
}

pub(crate) fn annotate(
    image: &mut Image,
    keyframe: &Keyframe,
    recording: &Recording,
    marks: &[Mark],
    typeface: &Typeface,
) -> Drawn {
    let shown = Shown::on(keyframe, recording, marks);
    let (width, height) = (image.width() as f32, image.height() as f32);
    let detail = ink::detail(width, height);
    let pen = ink::pen(width, height);
    let mut occupied: Vec<Point> = Vec::new();
    let mut extent: Option<Rect> = None;
    let mut cover = |pixels: Rect| {
        let part = Rect {
            left: pixels.left / width,
            top: pixels.top / height,
            right: pixels.right / width,
            bottom: pixels.bottom / height,
        };
        extent = Some(extent.map_or(part, |extent| extent.union(part)));
    };
    let mut canvas = image.as_mut();
    for path in &shown.ink {
        let pixels: Vec<Point> = path
            .iter()
            .map(|point| point.scaled(width, height))
            .collect();
        let outline = ink::outline(&pixels, pen);
        if let Some(outlined) = Rect::bounding(outline.iter().copied()) {
            cover(outlined);
        }
        fill(&mut canvas, closed_curve(&outline), ink::COLOR);
        occupied.extend(path);
    }
    let bounds = Rect::from_origin(Point::default(), width, height);
    if let Some(point) = shown.focus {
        let center = point.scaled(width, height);
        ripple(&mut canvas, center, detail);
        cover(
            Rect::at(center)
                .expanded_within((CLICK_RING + CLICK_LINE + CLICK_EDGE) * detail, bounds),
        );
    }
    let radius = ink::label_radius(width, height);
    for mark in &shown.labels {
        let center = mark.label_center(width, height, radius);
        label(&mut canvas, mark, center, radius, typeface);
        cover(Rect::at(center).expanded_within(radius, bounds));
        if let Shape::Pin { point } = mark.shape {
            cover(
                Rect::at(point.scaled(width, height))
                    .expanded_within(ink::PIN_DOT * radius, bounds),
            );
        }
        occupied.push(Point::new(center.x / width, center.y / height));
    }
    Drawn {
        occupied,
        focus: shown.focus,
        extent,
    }
}

fn nearest_click(actions: &[Action], moment: Moment) -> Option<Point> {
    actions
        .iter()
        .filter_map(|action| match *action {
            Action::Click { at, point } => Some((at.distance(moment), point)),
            Action::Drag { .. } => None,
        })
        .filter(|&(distance, _)| distance <= RIPPLE_REACH)
        .min_by_key(|&(distance, _)| distance)
        .map(|(_, point)| point)
}

fn ripple(canvas: &mut PixmapMut<'_>, center: Point, scale: f32) {
    let edge_outside = |boundary: f32| (boundary + CLICK_EDGE / 2.0, CLICK_EDGE, CLICK_WHITE);
    for (radius, width, color) in [
        (CLICK_RING, CLICK_LINE, CLICK_BLUE),
        edge_outside(CLICK_RING + CLICK_LINE / 2.0),
        edge_outside(CLICK_DOT),
    ] {
        let style = tiny_skia::Stroke {
            width: width * scale,
            ..tiny_skia::Stroke::default()
        };
        if let Some(circle) = PathBuilder::from_circle(center.x, center.y, radius * scale) {
            canvas.stroke_path(&circle, &paint(color), &style, Transform::identity(), None);
        }
    }
    fill(
        canvas,
        PathBuilder::from_circle(center.x, center.y, CLICK_DOT * scale),
        CLICK_BLUE,
    );
}

fn label(canvas: &mut PixmapMut<'_>, mark: &Mark, center: Point, radius: f32, typeface: &Typeface) {
    if let Shape::Pin { point } = mark.shape {
        let dot = ink::PIN_DOT * radius;
        let point = point.scaled(canvas.width() as f32, canvas.height() as f32);
        disc(canvas, point, dot, dot * (1.0 - 2.0 * ink::LABEL_RING));
    }
    disc(canvas, center, radius, radius * (1.0 - ink::LABEL_RING));
    let digits = mark.number.to_string();
    let natural = ink::LABEL_TYPE * radius;
    let size = natural * (ink::LABEL_DIGITS * radius / typeface.width(&digits, natural)).min(1.0);
    let origin = Point::new(
        center.x - typeface.width(&digits, size) / 2.0,
        center.y + typeface.cap_height(size) / 2.0,
    );
    fill(canvas, typeface.path(&digits, size, origin), WHITE);
}

fn disc(canvas: &mut PixmapMut<'_>, center: Point, radius: f32, inner: f32) {
    fill(
        canvas,
        PathBuilder::from_circle(center.x, center.y, radius),
        WHITE,
    );
    fill(
        canvas,
        PathBuilder::from_circle(center.x, center.y, inner),
        ink::COLOR,
    );
}

pub(crate) fn badge(
    canvas: &mut PixmapMut<'_>,
    cell: Rect,
    Badge { number, time }: &Badge,
    scale: f32,
    corner: Corner,
    typeface: &Typeface,
) {
    let number = format!("#{number}");
    let (number_size, time_size) = (15.5 * scale, 12.5 * scale);
    let height = 28.0 * scale;
    let radius = 8.5 * scale;
    let inset = 2.5 * scale;
    let keycap_height = height - 2.0 * inset;
    let number_width = typeface.width(&number, number_size);
    let keycap_width = (number_width + 12.0 * scale).max(keycap_height);
    let time_width = typeface.width(time, time_size);
    let gap = 7.0 * scale;
    let width = inset + keycap_width + gap + time_width + 9.0 * scale;
    let margin = 7.0 * scale;
    let left = match corner {
        Corner::Left => cell.left + margin,
        Corner::Right => (cell.right - width - margin).max(cell.left + margin),
    };
    let top = cell.top + margin;
    let outer = Rect::from_origin(Point::new(left, top), width, height);

    let shadow = Rect::from_origin(
        Point::new(left - scale, top - scale),
        width + 2.0 * scale,
        height + 2.0 * scale,
    );
    fill(canvas, rounded(shadow, radius + scale), [0, 0, 0, 96]);
    fill(canvas, rounded(outer, radius), [255, 255, 255, 78]);
    let border = (1.2 * scale).max(1.0);
    let body = Rect::from_origin(
        Point::new(left + border, top + border),
        (width - 2.0 * border).max(0.0),
        (height - 2.0 * border).max(0.0),
    );
    fill(
        canvas,
        rounded(body, (radius - border).max(0.0)),
        [14, 15, 18, 240],
    );
    let keycap = Rect::from_origin(
        Point::new(left + inset, top + inset),
        keycap_width,
        keycap_height,
    );
    fill(
        canvas,
        rounded(keycap, (radius - inset).max(2.0)),
        [248, 249, 252, 255],
    );
    let center_line = top + height / 2.0;
    let number_origin = Point::new(
        keycap.left + (keycap_width - number_width) / 2.0,
        center_line + typeface.cap_height(number_size) / 2.0,
    );
    fill(
        canvas,
        typeface.path(&number, number_size, number_origin),
        [14, 15, 18, 255],
    );
    let time_origin = Point::new(
        keycap.right + gap,
        center_line + typeface.cap_height(time_size) / 2.0,
    );
    fill(
        canvas,
        typeface.path(time, time_size, time_origin),
        [236, 238, 244, 255],
    );
}

pub(crate) fn badge_frame(
    image: &mut Image,
    keyframe: &Keyframe,
    corner: Corner,
    typeface: &Typeface,
) {
    let (width, height) = (image.width() as f32, image.height() as f32);
    let scale = (height / 480.0).min(width / 280.0).max(0.45);
    let whole = Rect::from_origin(Point::default(), width, height);
    badge(
        &mut image.as_mut(),
        whole,
        &Badge {
            number: keyframe.index,
            time: keyframe.at.to_string(),
        },
        scale,
        corner,
        typeface,
    );
}

pub(crate) fn paint(color: [u8; 4]) -> Paint<'static> {
    let [red, green, blue, alpha] = color;
    let mut paint = Paint::default();
    paint.set_color_rgba8(red, green, blue, alpha);
    paint
}

fn fill(canvas: &mut PixmapMut<'_>, path: Option<Path>, color: [u8; 4]) {
    if let Some(path) = path {
        canvas.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

pub(crate) fn rounded(rect: Rect, radius: f32) -> Option<Path> {
    let radius = radius
        .max(0.0)
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0);
    let handle = radius * QUARTER_CIRCLE_HANDLE;
    let Rect {
        left,
        top,
        right,
        bottom,
    } = rect;
    let mut builder = PathBuilder::new();
    builder.move_to(left + radius, top);
    builder.line_to(right - radius, top);
    builder.cubic_to(
        right - radius + handle,
        top,
        right,
        top + radius - handle,
        right,
        top + radius,
    );
    builder.line_to(right, bottom - radius);
    builder.cubic_to(
        right,
        bottom - radius + handle,
        right - radius + handle,
        bottom,
        right - radius,
        bottom,
    );
    builder.line_to(left + radius, bottom);
    builder.cubic_to(
        left + radius - handle,
        bottom,
        left,
        bottom - radius + handle,
        left,
        bottom - radius,
    );
    builder.line_to(left, top + radius);
    builder.cubic_to(
        left,
        top + radius - handle,
        left + radius - handle,
        top,
        left + radius,
        top,
    );
    builder.close();
    builder.finish()
}

fn closed_curve(points: &[Point]) -> Option<Path> {
    let (&first, &last) = (points.first()?, points.last()?);
    let start = last.lerp(first, 0.5);
    let mut builder = PathBuilder::new();
    builder.move_to(start.x, start.y);
    for (control, next) in points.iter().zip(points.iter().skip(1).chain([&first])) {
        let end = control.lerp(*next, 0.5);
        builder.quad_to(control.x, control.y, end.x, end.y);
    }
    builder.close();
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Reason, Size, marks, recording::Pin, typeface::tests::typeface};

    #[test]
    fn a_pin_is_numbered_on_its_keyframe_and_on_frames_just_after() {
        let (width, height) = (400_u32, 300_u32);
        let point = Point::new(0.5, 0.5);
        let recording = Recording {
            surface: Size {
                width: 800.0,
                height: 600.0,
            },
            pins: vec![Pin {
                at: Moment::from_millis(1_000),
                point,
            }],
            ..Recording::default()
        };
        let marks = marks::group(&recording.strokes, &recording.pins, recording.surface);
        let (w, h) = (width as f32, height as f32);
        let radius = ink::label_radius(w, h);
        let disc = marks[0].label_center(w, h, radius) + Point::new(0.0, -0.7 * radius);
        let inked = |at: u64, own| {
            let keyframe = Keyframe {
                index: 1,
                at: Moment::from_millis(at),
                reason: Reason::Mark,
                event: String::new(),
                said: None,
                marks: own,
                action: None,
                crop: None,
            };
            let mut image = Image::filled(width, height, [128; 3]).unwrap();
            annotate(&mut image, &keyframe, &recording, &marks, &typeface());
            let pixels = image.as_ref().data().as_chunks::<4>().0;
            [point.scaled(w, h), disc].map(|spot| {
                pixels.iter().enumerate().any(|(index, pixel)| {
                    let (x, y) = (index % width as usize, index / width as usize);
                    let center = Point::new(x as f32 + 0.5, y as f32 + 0.5);
                    center.distance(spot) < 1.0 && pixel[..3] == ink::COLOR[..3]
                })
            })
        };
        assert_eq!(inked(1_000, 0..1), [true, true], "Its own keyframe");
        assert_eq!(inked(2_000, 0..0), [true, true], "Just after");
        assert_eq!(inked(3_000, 0..0), [false, false], "Long after");
    }

    fn clicked(screen: [u8; 3]) -> Vec<(Point, [u8; 4])> {
        let (width, height) = (160_u32, 100_u32);
        let point = Point::new(0.5, 0.5);
        let keyframe = Keyframe {
            index: 1,
            at: Moment::from_millis(1_250),
            reason: Reason::Click,
            event: String::new(),
            said: None,
            marks: 0..0,
            action: Some(Action::Click {
                at: Moment::from_millis(1_000),
                point,
            }),
            crop: None,
        };
        let mut image = Image::filled(width, height, screen).unwrap();
        annotate(
            &mut image,
            &keyframe,
            &Recording::default(),
            &[],
            &typeface(),
        );
        let click = point.scaled(width as f32, height as f32);
        image
            .rows()
            .enumerate()
            .flat_map(|(y, row)| {
                row.as_chunks::<4>()
                    .0
                    .iter()
                    .enumerate()
                    .map(move |(x, &pixel)| {
                        (Point::new(x as f32 + 0.5, y as f32 + 0.5) - click, pixel)
                    })
            })
            .collect()
    }

    fn contrast(one: [u8; 3], other: [u8; 3]) -> f32 {
        let luminance = |color: [u8; 3]| {
            let [red, green, blue] = color.map(|channel| {
                let value = f32::from(channel) / 255.0;
                if value <= 0.040_45 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            });
            0.2126 * red + 0.7152 * green + 0.0722 * blue + 0.05
        };
        let (one, other) = (luminance(one), luminance(other));
        one.max(other) / one.min(other)
    }

    #[test]
    fn a_click_mark_leaves_what_was_clicked_legible() {
        let mut painted = 0;
        for ((offset, dark), (_, light)) in clicked([0; 3]).into_iter().zip(clicked([255; 3])) {
            let touched = dark[..3] != [0; 3] || light[..3] != [255; 3];
            painted += usize::from(touched);
            assert_ne!(dark, light, "{offset:?} from the click hides the screen");
            assert!(
                !(touched && (4.0..7.0).contains(&offset.length())),
                "{offset:?} from the click, between the dot and the ring, is covered"
            );
        }
        assert!(painted > 0, "The click is marked");
    }

    #[test]
    fn a_click_mark_stands_out_on_blue_light_dark_and_gray_screens() {
        use std::f32::consts::TAU;

        for screen in [
            [10, 132, 255],
            [0, 122, 255],
            [0, 120, 215],
            [255; 3],
            [0; 3],
            [128; 3],
        ] {
            let mut standing_out: Vec<f32> = clicked(screen)
                .into_iter()
                .filter(|&(offset, [red, green, blue, _])| {
                    offset.length() >= 7.0 && contrast([red, green, blue], screen) >= 3.0
                })
                .map(|(offset, _)| offset.y.atan2(offset.x))
                .collect();
            standing_out.sort_by(f32::total_cmp);
            let across_the_seam = standing_out
                .first()
                .zip(standing_out.last())
                .map_or(TAU, |(first, last)| first + TAU - last);
            let widest_gap = standing_out
                .iter()
                .zip(standing_out.iter().skip(1))
                .map(|(one, next)| next - one)
                .fold(across_the_seam, f32::max);
            assert!(
                widest_gap < TAU / 24.0,
                "On {screen:?}, the ring fades into the screen over {}°",
                widest_gap.to_degrees()
            );
        }
    }
}
