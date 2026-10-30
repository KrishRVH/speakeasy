//! A scripted Show of a settings window, for previews and tests: deterministic, with no capture.

use std::time::Duration;

use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, Path, PathBuilder, PixmapMut, SpreadMode,
    Transform,
};

use crate::{
    Action, Bundle, Error, Image, InkPoint, Moment, Narration, Pin, Point, Recording, Rect,
    SAMPLE_INTERVAL, Size, Span, Stroke, Typeface, Word, compose,
    draw::{paint, rounded},
    select,
    signature::{Sample, Signature},
};

/// The demo renders frames this small just to fingerprint them; a fingerprint's 128 × 72 grid
/// needs no more, and rendering costs grow with every pixel.
const SAMPLE_SIZE: (u32, u32) = (320, 200);
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
const CORNER_RADIUS: f32 = 14.0;
const SIDEBAR_RIGHT: f32 = 380.0;
const TITLE_BAR: f32 = 44.0;
const SECTIONS: [&str; 4] = ["General", "Shortcuts", "Audio", "Appearance"];
/// Where General and Appearance sit in [`SECTIONS`].
const GENERAL: usize = 0;
const APPEARANCE: usize = 3;
const ROW_TOP: f32 = 230.0;
const ROW_HEIGHT: f32 = 78.0;
const CONTROL_RIGHT: f32 = 1240.0;
const SLIDER_LEFT: f32 = 900.0;
const SLIDER_Y: f32 = row_center(2);
/// The slider's value where the drag picks it up and where it lets go.
const SLIDER_FROM: f32 = 0.35;
const SLIDER_TO: f32 = 0.7;

const ACCENT: [u8; 4] = [10, 132, 255, 255];
const TEXT: [u8; 4] = [28, 28, 30, 255];
const SECONDARY: [u8; 4] = [110, 110, 118, 255];
const TRACK: [u8; 4] = [210, 210, 216, 255];
const CHROME: [u8; 4] = [233, 233, 238, 255];

const OPEN_APPEARANCE: Moment = Moment::from_millis(2_500);
const APPEARANCE_SHOWN: Moment = Moment::from_millis(2_800);
const TOGGLE_CLICK: Moment = Moment::from_millis(12_000);
const TOGGLE_PIN: Moment = Moment::from_millis(12_600);
const DRAG_START: Moment = Moment::from_millis(16_000);
const DRAG_END: Moment = Moment::from_millis(17_000);
const END: Moment = Moment::from_millis(20_000);

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
/// toggle ignores a click and is pinned, and a slider follows a drag.
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

    /// The screen at `at`, rendered at `width × height` pixels; fails when a dimension is zero or
    /// too large to allocate.
    fn frame(&self, at: Moment, (width, height): (u32, u32)) -> Result<Image, Error> {
        let mut image = Image::filled(width, height, [20, 22, 28])?;
        let scale = Transform::from_scale(width as f32 / SCENE.width, height as f32 / SCENE.height);
        let mut scene = Scene {
            canvas: image.as_mut(),
            transform: scale,
            typeface: &self.typeface,
        };
        scene.draw(at);
        Ok(image)
    }

    /// The scripted Show as a bundle, sampled every [`SAMPLE_INTERVAL`] as a recorded Show is,
    /// with keyframes rendered at `keyframe_size` pixels.
    ///
    /// # Errors
    /// Fails when a keyframe dimension is zero or too large to allocate, or the script records no
    /// frames.
    pub fn bundle(&self, keyframe_size: (u32, u32)) -> Result<Bundle, Error> {
        let recording = recording();
        let samples = std::iter::successors(Some(Moment::START), |at| {
            Some(at.saturating_add(SAMPLE_INTERVAL)).filter(|next| *next <= recording.duration)
        })
        .map(|at| {
            let frame = self.frame(at, SAMPLE_SIZE)?;
            Ok(Sample {
                at,
                signature: Signature::of(&frame),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
        let selection = select(&recording, &samples).ok_or_else(Error::unsampled)?;
        compose(
            recording,
            selection,
            |at| self.frame(at, keyframe_size),
            &self.typeface,
        )
    }
}

struct Scene<'a> {
    canvas: PixmapMut<'a>,
    transform: Transform,
    typeface: &'a Typeface,
}

impl Scene<'_> {
    fn draw(&mut self, at: Moment) {
        self.backdrop();
        self.fill(rounded(WINDOW, CORNER_RADIUS), [245, 245, 247, 255]);
        let title_bar = Rect {
            bottom: WINDOW.top + TITLE_BAR,
            ..WINDOW
        };
        self.fill(rounded(title_bar, CORNER_RADIUS), CHROME);
        let square_base = Rect {
            top: title_bar.bottom - CORNER_RADIUS,
            ..title_bar
        };
        self.fill(rounded(square_base, 0.0), CHROME);
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
        let section = if at >= APPEARANCE_SHOWN {
            APPEARANCE
        } else {
            GENERAL
        };
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
        if section == GENERAL {
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
                GradientStop::new(0.0, Color::from_rgba8(28, 33, 48, 255)),
                GradientStop::new(1.0, Color::from_rgba8(14, 15, 20, 255)),
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

    fn appearance(&mut self, at: Moment) {
        self.text("Appearance", 28.0, Point::new(420.0, 190.0), TEXT);
        self.row(0, "Theme");
        let menu = Rect::from_origin(Point::new(960.0, row_center(0) - 20.0), 280.0, 40.0);
        self.fill(rounded(menu, 8.0), [255; 4]);
        self.text(
            "Daylight",
            17.0,
            Point::new(978.0, row_center(0) + 6.0),
            TEXT,
        );
        self.chevron(Point::new(1218.0, row_center(0)));
        self.row(1, "Reduce motion");
        self.toggle(row_center(1), false);
        self.row(2, "Text size");
        let value = slider_value(at);
        let track = Rect::from_origin(
            Point::new(SLIDER_LEFT, SLIDER_Y - 3.0),
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
            Point::new(slider_x(value) - 13.0, SLIDER_Y - 13.0),
            26.0,
            26.0,
        );
        self.fill(rounded(knob, 13.0), [255; 4]);
        let percent = format!("{}%", (80.0 + 80.0 * value).round());
        self.text(
            &percent,
            15.0,
            Point::new(SLIDER_LEFT - 70.0, SLIDER_Y + 5.0),
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
            let left = 1080.0 + 34.0 * index as f32;
            let swatch = Rect::from_origin(Point::new(left, row_center(3) - 12.0), 24.0, 24.0);
            self.fill(rounded(swatch, 12.0), color);
        }
        self.text(
            "Changes apply after you restart the app",
            16.0,
            Point::new(830.0, 768.0),
            SECONDARY,
        );
        let save = Rect::from_origin(Point::new(1120.0, 742.0), 120.0, 40.0);
        self.fill(rounded(save, 9.0), ACCENT);
        self.centered("Save", 17.0, save.center().x, 768.0, [255; 4]);
    }

    /// A small downward triangle, as a menu shows.
    fn chevron(&mut self, center: Point) {
        let mut builder = PathBuilder::new();
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
        self.fill(self.typeface.path(text, size, origin), color);
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
    160.0 + 44.0 * index as f32
}

const fn row_center(row: usize) -> f32 {
    ROW_TOP + ROW_HEIGHT * (row as f32 + 0.5)
}

fn slider_x(value: f32) -> f32 {
    SLIDER_LEFT + (CONTROL_RIGHT - SLIDER_LEFT) * value
}

/// The slider follows the drag, easing between where it started and where it was let go.
fn slider_value(at: Moment) -> f32 {
    let progress = at
        .since(DRAG_START)
        .div_duration_f32(DRAG_END.since(DRAG_START))
        .clamp(0.0, 1.0);
    let eased = progress * progress * (3.0 - 2.0 * progress);
    SLIDER_FROM * (1.0 - eased) + SLIDER_TO * eased
}

fn normalized(x: f32, y: f32) -> Point {
    Point::new(x / SCENE.width, y / SCENE.height)
}

/// Everything the scripted Show captured except its frames.
fn recording() -> Recording {
    Recording {
        duration: END,
        surface: SCENE,
        strokes: strokes(),
        pins: vec![Pin {
            at: TOGGLE_PIN,
            point: normalized(1212.0, row_center(1)),
        }],
        actions: vec![
            Action::Click {
                at: OPEN_APPEARANCE,
                point: normalized(270.0, section_center(APPEARANCE)),
            },
            Action::Click {
                at: TOGGLE_CLICK,
                point: normalized(1206.0, row_center(1)),
            },
            Action::Drag {
                span: Span::new(DRAG_START, DRAG_END),
                from: normalized(slider_x(SLIDER_FROM), SLIDER_Y),
                to: normalized(slider_x(SLIDER_TO), SLIDER_Y),
            },
        ],
        narration: narration(),
    }
}

/// A circle around the theme menu, then a three-stroke arrow at the label under the button.
fn strokes() -> Vec<Stroke> {
    let ink = |point: Point, at: u64| InkPoint {
        point: normalized(point.x, point.y),
        at: Moment::from_millis(at),
    };
    let circle = (0..=36_u8).map(|step| {
        let turn = f32::from(step) / 36.0;
        let angle = std::f32::consts::TAU * turn * 1.08 - 1.9;
        let wobble = 1.0 + 0.04 * (turn * 9.0).sin();
        let point = Point::new(
            1100.0 + 170.0 * wobble * angle.cos(),
            row_center(0) + 40.0 * wobble * angle.sin(),
        );
        let at = 5_600_u64.saturating_add(u64::from(step).saturating_mul(28));
        ink(point, at)
    });
    let line = |from: Point, to: Point, start: u64, steps: u8| {
        (0..=steps).map(move |step| {
            let point = from.lerp(to, f32::from(step) / f32::from(steps));
            let at = start.saturating_add(u64::from(step).saturating_mul(24));
            ink(point, at)
        })
    };
    let tip = Point::new(1110.0, 748.0);
    [
        circle.collect(),
        line(Point::new(900.0, 640.0), tip, 8_200, 20).collect(),
        line(Point::new(1082.0, 738.0), tip, 8_800, 6).collect(),
        line(Point::new(1100.0, 718.0), tip, 9_050, 6).collect(),
    ]
    .into_iter()
    .filter_map(Stroke::new)
    .collect()
}

/// Words spread over each phrase at a steady speaking pace.
fn narration() -> Narration {
    const PER_CHARACTER: Duration = Duration::from_millis(62);
    Narration::Words(
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
                        Some(Word {
                            span: Span::new(std::mem::replace(at, end), end),
                            text: text.to_owned(),
                        })
                    })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::typeface::tests::typeface;

    #[test]
    fn the_demo_tells_its_story_in_four_keyframes() {
        let bundle = Demo::new(typeface()).bundle((720, 450)).unwrap();
        let expected = bundle.manifest.clone();
        let directory = Path::new("shows").join("demo");
        let (summary, files) = bundle.into_files(&directory).unwrap();
        assert_eq!(
            summary,
            format!(
                "Screen recording (0:20, 4 keyframes): `{}`
Before acting, open the contact sheet, then the full-size frames that matter: they show what the words leave out. Speech recognition can mishear a name; where the screen spells it differently, trust the screen. Cite frames as #N and marks by their circled numbers.
Contact sheet: `{}`
- #1 (0:02) [clicked] “Here's the settings window. When I open Appearance,”
- #2 (0:09) [① circled, ② drew an arrow] “the theme picker is here, but look, this label runs under the save button.”
- #3 (0:12) [③ pinned, clicked] “Now I click reduce motion and nothing happens.”
- #4 (0:17) [dragged right] “Dragging the text size slider works, though.”
Full-size keyframes: `{}` to `{}`. Timeline: `{}`.
",
                directory.display(),
                directory.join("contact-sheet.png").display(),
                directory.join("frames").join("frame-01.png").display(),
                directory.join("frames").join("frame-04.png").display(),
                directory.join("show.json").display()
            )
        );
        let marks = &expected.marks;
        assert_eq!(marks.len(), 3);
        assert_eq!(marks[0].shape, crate::Shape::Circle);
        assert!(
            matches!(marks[1].shape, crate::Shape::Arrow { .. }),
            "{:?}",
            marks[1].shape
        );
        assert!(
            matches!(marks[2].shape, crate::Shape::Pin { .. }),
            "{:?}",
            marks[2].shape
        );
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
        assert_eq!(files[5].contents, summary.as_bytes());
        let sheet = Image::decode_png(&files[0].contents);
        assert!(sheet.width().max(sheet.height()) <= 1_600);
        let manifest: crate::Manifest = serde_json::from_slice(&files[6].contents).unwrap();
        assert_eq!(manifest, expected);
    }
}
