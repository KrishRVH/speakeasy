//! Ink, numbered labels, click ripples, and badges drawn onto keyframes.

use std::time::Duration;

use tiny_skia::{FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke as Line, Transform};

use crate::{
    Image, Keyframe, Mark, Moment, Point, Rect, Shape, Span, Typeface, ink,
    recording::{Action, Recording},
};

const RIPPLE_FILL: [u8; 4] = [0, 136, 255, 68];
const RIPPLE_RING: [u8; 4] = [0, 136, 255, 235];
const WHITE: [u8; 4] = [255, 255, 255, 255];
/// Ink lifted this recently before a keyframe without marks of its own still shows on it, as do
/// pins dropped this recently.
const RECENT_INK: Duration = Duration::from_millis(1_200);
/// A keyframe without a click or drag of its own ripples at a click this close to its moment.
const RIPPLE_REACH: Duration = Duration::from_millis(600);
/// The top band where a badge sits; the left and right thirds decide which corner it takes.
const BADGE_BAND: f32 = 0.18;
const BADGE_SIDE: f32 = 0.30;

/// The top corner a keyframe's badge sits in.
#[derive(Clone, Copy)]
pub(crate) enum Corner {
    Left,
    Right,
}

/// Draws a keyframe's ink, numbered labels, and click ripple onto its frame, returning the corner
/// its badge should take so it does not cover what the keyframe is about.
///
/// A keyframe made for marks shows all their ink. Any other keyframe shows the ink drawn up to its
/// moment from strokes still in progress or lifted within [`RECENT_INK`], as the screen looked
/// then. Each of `marks`, the selection's, that the keyframe is for, or whose ink or pin it shows
/// that way, is labeled with its number. The ripple marks the keyframe's own click or drag, which a
/// mark's keyframe can carry too, else the click nearest its moment within [`RIPPLE_REACH`].
pub(crate) fn annotate(
    image: &mut Image,
    keyframe: &Keyframe,
    recording: &Recording,
    marks: &[Mark],
    typeface: &Typeface,
) -> Corner {
    let (width, height) = (image.width() as f32, image.height() as f32);
    let detail = ink::detail(width, height);
    let pen = ink::pen(width, height);
    let own = marks.get(keyframe.marks.clone()).unwrap_or_default();
    let shown = |index: usize| own.iter().any(|mark| mark.strokes.contains(&index));
    // Marks are spans of strokes drawn at most this far apart, so a mark is recent exactly when
    // one of its strokes is.
    let recent =
        |span: Span| span.start() <= keyframe.at && keyframe.at.since(span.end()) <= RECENT_INK;
    let mut ink_points: Vec<Point> = Vec::new();
    let mut canvas = image.as_mut();
    for (index, stroke) in recording.strokes.iter().enumerate() {
        let path: Vec<Point> = if shown(index) {
            stroke.path().collect()
        } else if own.is_empty() && recent(stroke.span()) {
            stroke.path_until(keyframe.at).collect()
        } else {
            continue;
        };
        let pixels: Vec<Point> = path
            .iter()
            .map(|point| point.scaled(width, height))
            .collect();
        if let Some(outline) = closed_curve(&ink::outline(&pixels, pen)) {
            canvas.fill_path(
                &outline,
                &paint(ink::COLOR),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        ink_points.extend(path);
    }
    let click = nearest_click(&recording.actions, keyframe.at);
    let focus = keyframe.action.map(Action::point).or(click);
    if let Some(point) = focus.filter(|point| point.is_finite()) {
        ripple(&mut canvas, point.scaled(width, height), detail);
    }
    let radius = ink::label_radius(width, height);
    let labeled = marks
        .iter()
        .filter(|mark| own.contains(mark) || (own.is_empty() && recent(mark.span)));
    for mark in labeled {
        let center = mark.label(width, height, radius);
        label(&mut canvas, mark, center, radius, typeface);
        ink_points.push(Point::new(center.x / width, center.y / height));
    }
    let in_top_left = |point: &Point| point.x < BADGE_SIDE && point.y < BADGE_BAND;
    let in_top_right = |point: &Point| point.x > 1.0 - BADGE_SIDE && point.y < BADGE_BAND;
    let crowded_left = focus.iter().chain(&ink_points).any(in_top_left);
    if crowded_left && !focus.iter().any(in_top_right) {
        Corner::Right
    } else {
        Corner::Left
    }
}

fn nearest_click(actions: &[Action], moment: Moment) -> Option<Point> {
    actions
        .iter()
        .filter_map(|action| match *action {
            Action::Click { at, point } if at.distance(moment) <= RIPPLE_REACH => {
                Some((at.distance(moment), point))
            },
            Action::Click { .. } | Action::Drag { .. } => None,
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, point)| point)
}

/// A translucent disc inside white and blue rings, marking where the pointer acted.
fn ripple(canvas: &mut PixmapMut<'_>, center: Point, scale: f32) {
    if let Some(disc) = PathBuilder::from_circle(center.x, center.y, 14.0 * scale) {
        canvas.fill_path(
            &disc,
            &paint(RIPPLE_FILL),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    for (radius, color) in [(15.0, WHITE), (17.5, RIPPLE_RING)] {
        if let Some(ring) = PathBuilder::from_circle(center.x, center.y, radius * scale) {
            let line = Line {
                width: 1.75 * scale,
                ..Line::default()
            };
            canvas.stroke_path(&ring, &paint(color), &line, Transform::identity(), None);
        }
    }
}

/// Draws `mark`'s number in white on a red disc ringed in white, centered at `center` in pixels,
/// with a dot at the point a pin marks.
fn label(canvas: &mut PixmapMut<'_>, mark: &Mark, center: Point, radius: f32, typeface: &Typeface) {
    let width = canvas.width() as f32;
    let height = canvas.height() as f32;
    if let Shape::Pin { point } = mark.shape {
        let point = point.scaled(width, height);
        let dot = ink::PIN_DOT * radius;
        fill(
            canvas,
            PathBuilder::from_circle(point.x, point.y, dot),
            WHITE,
        );
        fill(
            canvas,
            PathBuilder::from_circle(point.x, point.y, dot * (1.0 - 2.0 * ink::LABEL_RING)),
            ink::COLOR,
        );
    }
    fill(
        canvas,
        PathBuilder::from_circle(center.x, center.y, radius),
        WHITE,
    );
    fill(
        canvas,
        PathBuilder::from_circle(center.x, center.y, radius * (1.0 - ink::LABEL_RING)),
        ink::COLOR,
    );
    let digits = mark.number.to_string();
    let natural = ink::LABEL_TYPE * radius;
    let size = natural * (ink::LABEL_DIGITS * radius / typeface.width(&digits, natural)).min(1.0);
    let origin = Point::new(
        center.x - typeface.width(&digits, size) / 2.0,
        center.y + typeface.cap_height(size) / 2.0,
    );
    fill(canvas, typeface.path(&digits, size, origin), WHITE);
}

/// Draws a badge such as `#3  0:42` in the top corner of `cell`, a rectangle in pixels.
///
/// The number sits on a light keycap so a reader can refer to the keyframe; the time beside it
/// says when it happened. `scale` sizes the badge for the cell it labels.
pub(crate) fn badge(
    canvas: &mut PixmapMut<'_>,
    cell: Rect,
    keyframe: &Keyframe,
    scale: f32,
    corner: Corner,
    typeface: &Typeface,
) {
    let number = format!("#{}", keyframe.index);
    let time = keyframe.at.to_string();
    let (number_size, time_size) = (15.5 * scale, 12.5 * scale);
    let height = 28.0 * scale;
    let radius = 8.5 * scale;
    let inset = 2.5 * scale;
    let cap_height = height - 2.0 * inset;
    let number_width = typeface.width(&number, number_size);
    let cap_width = (number_width + 12.0 * scale).max(cap_height);
    let time_width = typeface.width(&time, time_size);
    let width = inset + cap_width + 7.0 * scale + time_width + 9.0 * scale;
    let margin = 7.0 * scale;
    let left = match corner {
        Corner::Left => cell.left + margin,
        Corner::Right => (cell.right - width - margin).max(cell.left + margin),
    };
    let top = cell.top + margin;
    let frame = Rect::from_origin(Point::new(left, top), width, height);

    let shadow = Rect::from_origin(
        Point::new(left - scale, top - scale),
        width + 2.0 * scale,
        height + 2.0 * scale,
    );
    fill(canvas, rounded(shadow, radius + scale), [0, 0, 0, 96]);
    fill(canvas, rounded(frame, radius), [255, 255, 255, 78]);
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
    let cap = Rect::from_origin(Point::new(left + inset, top + inset), cap_width, cap_height);
    fill(
        canvas,
        rounded(cap, (radius - inset).max(2.0)),
        [248, 249, 252, 255],
    );
    let center_line = top + height / 2.0;
    let number_origin = Point::new(
        cap.left + (cap_width - number_width) / 2.0,
        center_line + typeface.cap_height(number_size) / 2.0,
    );
    fill(
        canvas,
        typeface.path(&number, number_size, number_origin),
        [14, 15, 18, 255],
    );
    let time_origin = Point::new(
        cap.right + 7.0 * scale,
        center_line + typeface.cap_height(time_size) / 2.0,
    );
    fill(
        canvas,
        typeface.path(&time, time_size, time_origin),
        [236, 238, 244, 255],
    );
}

/// How large a badge reads well on a frame of this size.
pub(crate) fn badge_scale(width: f32, height: f32) -> f32 {
    (height / 480.0).min(width / 280.0).max(0.45)
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

/// A rectangle with circular corners of `radius`, clamped to fit.
pub(crate) fn rounded(rect: Rect, radius: f32) -> Option<Path> {
    let radius = radius
        .max(0.0)
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0);
    // A cubic Bézier approximates a quarter circle with its handles this far along the tangent.
    let handle = radius * 0.552_284_8;
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

/// A closed path through `points`, curving through the midpoints of its edges so an outline
/// computed as a polygon draws without facets.
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
        let disc = marks[0].label(w, h, radius) + Point::new(0.0, -0.7 * radius);
        let inked = |at: u64, own| {
            let keyframe = Keyframe {
                index: 1,
                at: Moment::from_millis(at),
                reason: Reason::Mark,
                event: String::new(),
                said: None,
                marks: own,
                action: None,
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
        assert_eq!(inked(1_000, 0..1), [true, true], "its own keyframe");
        assert_eq!(inked(2_000, 0..0), [true, true], "just after");
        assert_eq!(inked(3_000, 0..0), [false, false], "long after");
    }
}
