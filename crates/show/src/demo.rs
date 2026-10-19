//! A scripted Show of a settings window, for previews and tests: deterministic, with no capture.

use std::time::Duration;

use tiny_skia::{FillRule, LinearGradient, Paint, Path, PixmapMut, SpreadMode, Transform};

use crate::{
    Action, Error, Image, InkPoint, Moment, Point, Recording, Size, Span, Speech, Stroke, Typeface,
    Word,
    draw::{paint, rounded},
    geometry::Rect,
};

/// The scene's design size in logical pixels; frames scale it to any resolution.
const SCENE: Size = Size {
    width: 1440.0,
    height: 900.0,
};
const WINDOW: Rect = Rect {
    left: 160.0,
    top: 90.0,
    right: 1280.0,
    bottom: 810.0,
};
const SIDEBAR_RIGHT: f32 = 380.0;
const TITLE_BAR: f32 = 44.0;
const SECTIONS: [&str; 4] = ["General", "Shortcuts", "Audio", "Appearance"];
const ROW_TOP: f32 = 230.0;
const ROW_HEIGHT: f32 = 78.0;
const CONTROL_RIGHT: f32 = 1240.0;
const SLIDER_LEFT: f32 = 900.0;

const ACCENT: [u8; 4] = [10, 132, 255, 255];
const TEXT: [u8; 4] = [28, 28, 30, 255];
const SECONDARY: [u8; 4] = [110, 110, 118, 255];
const TRACK: [u8; 4] = [210, 210, 216, 255];

/// When things happen, in milliseconds from the start.
const OPEN_APPEARANCE: u64 = 2_500;
const APPEARANCE_SHOWN: u64 = 2_800;
const TOGGLE_CLICK: u64 = 12_000;
const DRAG: (u64, u64) = (16_000, 17_000);
const END: u64 = 20_000;

/// The narration, as phrases starting at a moment.
const NARRATION: [(u64, &str); 6] = [
    (400, "Here's the settings window."),
    (2_200, "When I open Appearance,"),
    (5_000, "the theme picker is here,"),
    (7_600, "but look, this label runs under the save button."),
    (11_200, "Now I click reduce motion and nothing happens."),
    (15_600, "Dragging the text size slider works, though."),
];

/// A scripted Show of a settings window, narrated throughout.
///
/// A click opens a section, ink circles a menu and points at a label running under a button, a
/// toggle ignores a click, and a slider follows a drag.
#[derive(Clone, Debug)]
pub struct Demo {
    typeface: Typeface,
}

impl Demo {
    /// A demo whose interface text is drawn in `typeface`.
    #[must_use]
    pub const fn new(typeface: Typeface) -> Self {
        Self { typeface }
    }

    /// Everything the scripted Show captured except its frames.
    #[must_use]
    pub fn recording(&self) -> Recording {
        Recording {
            duration: Moment::from_millis(END),
            surface: SCENE,
            strokes: strokes(),
            actions: vec![
                Action::Click {
                    at: Moment::from_millis(OPEN_APPEARANCE),
                    point: normalized(270.0, section_center(3)),
                },
                Action::Click {
                    at: Moment::from_millis(TOGGLE_CLICK),
                    point: normalized(1_206.0, row_center(1)),
                },
                Action::Drag {
                    span: Span::new(Moment::from_millis(DRAG.0), Moment::from_millis(DRAG.1)),
                    from: normalized(slider_x(0.35), slider_y()),
                    to: normalized(slider_x(0.7), slider_y()),
                },
            ],
            speech: narration(),
        }
    }

    /// The screen at `at`, rendered at `width × height` pixels.
    ///
    /// # Errors
    ///
    /// Fails when a dimension is zero or too large to allocate.
    pub fn frame(&self, at: Moment, width: u32, height: u32) -> Result<Image, Error> {
        let mut image = Image::filled(width, height, [20, 22, 28])?;
        let scale = Transform::from_scale(width as f32 / SCENE.width, height as f32 / SCENE.height);
        let mut scene = Scene {
            canvas: image.as_mut(),
            transform: scale,
            typeface: &self.typeface,
        };
        scene.draw(at.as_millis());
        Ok(image)
    }
}

struct Scene<'a, 'b> {
    canvas: PixmapMut<'a>,
    transform: Transform,
    typeface: &'b Typeface,
}

impl Scene<'_, '_> {
    fn draw(&mut self, at: u64) {
        self.backdrop();
        self.fill(rounded(WINDOW, 14.0), [245, 245, 247, 255]);
        let title_bar = Rect {
            bottom: WINDOW.top + TITLE_BAR,
            ..WINDOW
        };
        self.fill(rounded(title_bar, 14.0), [233, 233, 238, 255]);
        self.fill(
            rounded(
                Rect {
                    top: title_bar.bottom - 14.0,
                    ..title_bar
                },
                0.0,
            ),
            [233, 233, 238, 255],
        );
        self.centered(
            "Settings",
            17.0,
            WINDOW.center().x,
            WINDOW.top + 28.0,
            SECONDARY,
        );
        let sidebar = Rect {
            top: WINDOW.top + TITLE_BAR,
            right: SIDEBAR_RIGHT,
            ..WINDOW
        };
        self.fill(rounded(sidebar, 0.0), [236, 236, 241, 255]);
        let section = if at >= APPEARANCE_SHOWN { 3 } else { 0 };
        for (index, name) in SECTIONS.iter().enumerate() {
            let center = section_center(index);
            let selected = index == section;
            if selected {
                let pill = Rect::from_origin(Point::new(172.0, center - 18.0), 196.0, 36.0);
                self.fill(rounded(pill, 9.0), ACCENT);
            }
            let color = if selected { [255; 4] } else { TEXT };
            self.text(name, 17.0, Point::new(190.0, center + 6.0), color);
        }
        if section == 0 {
            self.general();
        } else {
            self.appearance(at);
        }
    }

    fn backdrop(&mut self) {
        let shader = LinearGradient::new(
            tiny_skia::Point::from_xy(0.0, 0.0),
            tiny_skia::Point::from_xy(SCENE.width, SCENE.height),
            vec![
                tiny_skia::GradientStop::new(0.0, tiny_skia::Color::from_rgba8(28, 33, 48, 255)),
                tiny_skia::GradientStop::new(1.0, tiny_skia::Color::from_rgba8(14, 15, 20, 255)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        );
        if let (Some(shader), Some(path)) = (
            shader,
            rounded(Rect::from([0.0, 0.0, SCENE.width, SCENE.height]), 0.0),
        ) {
            self.paint(
                &path,
                &Paint {
                    shader,
                    ..Paint::default()
                },
            );
        }
    }

    fn general(&mut self) {
        self.text("General", 28.0, Point::new(420.0, 190.0), TEXT);
        for (row, (label, on)) in [
            ("Launch at login", true),
            ("Play sounds", false),
            ("Check for updates", true),
        ]
        .into_iter()
        .enumerate()
        {
            self.row(row, label);
            self.toggle(row_center(row), on);
        }
    }

    fn appearance(&mut self, at: u64) {
        self.text("Appearance", 28.0, Point::new(420.0, 190.0), TEXT);
        self.row(0, "Theme");
        let menu = Rect::from_origin(Point::new(960.0, row_center(0) - 20.0), 280.0, 40.0);
        self.fill(rounded(menu, 8.0), [255; 4]);
        self.text(
            "Jet & Champagne",
            17.0,
            Point::new(978.0, row_center(0) + 6.0),
            TEXT,
        );
        self.chevron(Point::new(1_218.0, row_center(0)));
        self.row(1, "Reduce motion");
        // The bug the narration describes: the toggle ignores the click.
        self.toggle(row_center(1), false);
        self.row(2, "Text size");
        let value = slider_value(at);
        let track = Rect::from_origin(
            Point::new(SLIDER_LEFT, slider_y() - 3.0),
            CONTROL_RIGHT - SLIDER_LEFT,
            6.0,
        );
        self.fill(rounded(track, 3.0), TRACK);
        let filled = Rect {
            right: slider_x(value),
            ..track
        };
        self.fill(rounded(filled, 3.0), ACCENT);
        let knob = Rect::from_origin(
            Point::new(slider_x(value) - 13.0, slider_y() - 13.0),
            26.0,
            26.0,
        );
        self.fill(rounded(knob, 13.0), [255; 4]);
        let percent = format!("{}%", 80.0f32.mul_add(value, 80.0).round());
        self.text(
            &percent,
            15.0,
            Point::new(SLIDER_LEFT - 70.0, slider_y() + 5.0),
            SECONDARY,
        );
        self.row(3, "Accent");
        let swatches: [[u8; 4]; 5] = [
            ACCENT,
            [175, 82, 222, 255],
            [255, 69, 58, 255],
            [255, 159, 10, 255],
            [48, 209, 88, 255],
        ];
        for (index, color) in swatches.into_iter().enumerate() {
            let left = 34.0f32.mul_add(index as f32, 1_080.0);
            let swatch = Rect::from_origin(Point::new(left, row_center(3) - 12.0), 24.0, 24.0);
            self.fill(rounded(swatch, 12.0), color);
        }
        // The label runs under the Save button.
        self.text(
            "Changes apply after you restart Speakeasy",
            16.0,
            Point::new(830.0, 768.0),
            SECONDARY,
        );
        let save = Rect::from_origin(Point::new(1_120.0, 742.0), 120.0, 40.0);
        self.fill(rounded(save, 9.0), ACCENT);
        self.centered("Save", 17.0, save.center().x, 768.0, [255; 4]);
    }

    /// A small downward triangle, as a menu shows.
    fn chevron(&mut self, center: Point) {
        let mut builder = tiny_skia::PathBuilder::new();
        builder.move_to(center.x - 6.0, center.y - 3.0);
        builder.line_to(center.x + 6.0, center.y - 3.0);
        builder.line_to(center.x, center.y + 4.0);
        builder.close();
        self.fill(builder.finish(), SECONDARY);
    }

    fn row(&mut self, row: usize, label: &str) {
        let center = row_center(row);
        self.text(label, 19.0, Point::new(420.0, center + 7.0), TEXT);
        let rule = Rect::from_origin(
            Point::new(420.0, center + ROW_HEIGHT / 2.0),
            CONTROL_RIGHT - 420.0,
            1.0,
        );
        self.fill(rounded(rule, 0.0), [224, 224, 229, 255]);
    }

    fn toggle(&mut self, center: f32, on: bool) {
        let track = Rect::from_origin(Point::new(CONTROL_RIGHT - 52.0, center - 15.0), 52.0, 30.0);
        self.fill(
            rounded(track, 15.0),
            if on { [52, 199, 89, 255] } else { TRACK },
        );
        let left = if on {
            track.right - 28.0
        } else {
            track.left + 2.0
        };
        let knob = Rect::from_origin(Point::new(left, center - 13.0), 26.0, 26.0);
        self.fill(rounded(knob, 13.0), [255; 4]);
    }

    fn text(&mut self, text: &str, size: f32, origin: Point, color: [u8; 4]) {
        if let Some(path) = self.typeface.path(text, size, origin) {
            self.paint(&path, &paint(color));
        }
    }

    fn centered(&mut self, text: &str, size: f32, center: f32, baseline: f32, color: [u8; 4]) {
        let left = center - self.typeface.width(text, size) / 2.0;
        self.text(text, size, Point::new(left, baseline), color);
    }

    fn fill(&mut self, path: Option<Path>, color: [u8; 4]) {
        if let Some(path) = path {
            self.paint(&path, &paint(color));
        }
    }

    fn paint(&mut self, path: &Path, paint: &Paint<'_>) {
        self.canvas
            .fill_path(path, paint, FillRule::Winding, self.transform, None);
    }
}

fn section_center(index: usize) -> f32 {
    44.0f32.mul_add(index as f32, 160.0)
}

fn row_center(row: usize) -> f32 {
    ROW_HEIGHT.mul_add(row as f32, ROW_TOP + ROW_HEIGHT / 2.0)
}

fn slider_y() -> f32 {
    row_center(2)
}

fn slider_x(value: f32) -> f32 {
    (CONTROL_RIGHT - SLIDER_LEFT).mul_add(value, SLIDER_LEFT)
}

/// The slider follows the drag, easing between where it started and where it was let go.
fn slider_value(at: u64) -> f32 {
    let progress =
        (at.saturating_sub(DRAG.0) as f32 / DRAG.1.saturating_sub(DRAG.0) as f32).clamp(0.0, 1.0);
    let eased = progress * progress * 2.0f32.mul_add(-progress, 3.0);
    0.35f32.mul_add(1.0 - eased, 0.7 * eased)
}

fn normalized(x: f32, y: f32) -> Point {
    Point::new(x / SCENE.width, y / SCENE.height)
}

/// A circle around the theme menu, then an arrow, in three strokes, at the label under the button.
fn strokes() -> Vec<Stroke> {
    let circle = (0..=36_u8).map(|step| {
        let turn = f32::from(step) / 36.0;
        let angle = std::f32::consts::TAU.mul_add(turn * 1.08, -1.9);
        let wobble = 0.04f32.mul_add((turn * 9.0).sin(), 1.0);
        let point = Point::new(
            (170.0 * wobble).mul_add(angle.cos(), 1_100.0),
            (40.0 * wobble).mul_add(angle.sin(), row_center(0)),
        );
        (
            point,
            5_600_u64.saturating_add(u64::from(step).saturating_mul(28)),
        )
    });
    let line = |from: Point, to: Point, start: u64, steps: u8| {
        (0..=steps).map(move |step| {
            let progress = f32::from(step) / f32::from(steps);
            (
                from.lerp(to, progress),
                start.saturating_add(u64::from(step).saturating_mul(24)),
            )
        })
    };
    let tip = Point::new(1_110.0, 748.0);
    [
        circle.collect::<Vec<_>>(),
        line(Point::new(900.0, 640.0), tip, 8_200, 20).collect(),
        line(Point::new(1_082.0, 738.0), tip, 8_800, 6).collect(),
        line(Point::new(1_100.0, 718.0), tip, 9_050, 6).collect(),
    ]
    .into_iter()
    .filter_map(|points| {
        Stroke::new(
            points
                .into_iter()
                .map(|(point, at)| InkPoint {
                    point: normalized(point.x, point.y),
                    at: Moment::from_millis(at),
                })
                .collect(),
        )
    })
    .collect()
}

/// Words spread over each phrase at a steady speaking pace.
fn narration() -> Speech {
    const PER_CHARACTER: Duration = Duration::from_millis(62);
    Speech::Words(
        NARRATION
            .into_iter()
            .flat_map(|(start, phrase)| {
                phrase
                    .split_whitespace()
                    .scan(Moment::from_millis(start), |at, text| {
                        let length = u32::try_from(text.len())
                            .unwrap_or(u32::MAX)
                            .saturating_add(2);
                        let end = at.saturating_add(PER_CHARACTER.saturating_mul(length));
                        let word = Word {
                            span: Span::new(*at, end),
                            text: text.to_owned(),
                        };
                        *at = end;
                        Some(word)
                    })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{Sample, Signature, compose, select, typeface::tests::typeface};

    #[test]
    fn the_demo_tells_its_story_in_four_keyframes() {
        let demo = Demo::new(typeface());
        let recording = demo.recording();
        let samples: Vec<Sample> = (0..=END)
            .step_by(250)
            .map(|at| {
                let at = Moment::from_millis(at);
                let frame = demo.frame(at, 320, 200).unwrap();
                Sample {
                    at,
                    signature: Signature::of(&frame),
                }
            })
            .collect();
        let selection = select(
            &recording,
            &samples,
            crate::budget(recording.duration.since(Moment::START)),
        );
        let frames = selection
            .keyframes
            .iter()
            .map(|keyframe| demo.frame(keyframe.at, 720, 450).unwrap())
            .collect();
        let bundle = compose(recording, selection, frames, &typeface()).unwrap();
        let directory = Path::new("shows").join("demo");
        assert_eq!(
            bundle.summary(&directory),
            format!(
                "Screen recording (0:20, 4 keyframes): {}
Contact sheet: {}
- #1 (0:02) [clicked] “Here's the settings window. When I open Appearance,”
- #2 (0:09) [circled, drew an arrow] “the theme picker is here, but look, this label runs under the save button.”
- #3 (0:12) [clicked] “Now I click reduce motion and nothing happens.”
- #4 (0:17) [dragged right] “Dragging the text size slider works, though.”
Full-size keyframes: frames/frame-01.png to frames/frame-04.png. Timeline: show.json.
",
                directory.display(),
                directory.join("contact-sheet.png").display()
            )
        );
        let marks = &bundle.manifest().marks;
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].shape, crate::Shape::Circle);
        assert!(
            matches!(marks[1].shape, crate::Shape::Arrow { .. }),
            "{:?}",
            marks[1].shape
        );
        let files = bundle.files(&directory).unwrap();
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "contact-sheet.png",
                "frames/frame-01.png",
                "frames/frame-02.png",
                "frames/frame-03.png",
                "frames/frame-04.png",
                "summary.md",
                "show.json"
            ]
        );
        let sheet = Image::decode_png(&files[0].contents);
        assert!(sheet.width().max(sheet.height()) <= 1_600);
        let manifest: crate::Manifest = serde_json::from_slice(&files[6].contents).unwrap();
        assert_eq!(&manifest, bundle.manifest());
    }
}
