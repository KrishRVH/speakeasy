//! The finished Show: annotated keyframes, the contact sheet, the summary, and the manifest.

use std::{fmt, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    Error, Image, Keyframe, Mark, Point, Recording, Rect, Selection, Typeface,
    describe::{Count, Marker},
    draw::{self, Corner},
    parallel, sheet,
};

const CONTACT_SHEET: &str = "contact-sheet.png";
const SUMMARY: &str = "summary.md";
const MANIFEST: &str = "show.json";

impl Keyframe {
    /// The keyframe image's path within a bundle.
    #[must_use]
    pub fn file(&self) -> String {
        format!("frames/frame-{:02}.png", self.index)
    }
}

/// Everything a Show recorded and chose, as `show.json` stores it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// The format's version, which changes only when a reader must.
    pub version: u32,
    /// What was captured.
    #[serde(flatten)]
    pub recording: Recording,
    /// The ink grouped into gestures.
    pub marks: Vec<Mark>,
    /// The chosen frames, each stored at [`Keyframe::file`].
    pub keyframes: Vec<Keyframe>,
}

impl Manifest {
    /// The version this crate writes.
    pub const VERSION: u32 = 1;
}

/// A Show ready to write: its manifest and its images, annotated and badged.
#[derive(Clone, Debug)]
pub struct Bundle {
    manifest: Manifest,
    frames: Vec<Image>,
    contact_sheet: Image,
}

/// One file of a bundle, named by its path within the bundle's directory.
#[derive(Clone, PartialEq, Eq)]
pub struct File {
    /// The path within the bundle, with `/` separators.
    pub name: String,
    /// The file's bytes.
    pub contents: Vec<u8>,
}

/// Names the file and its size; its contents are screen images or narration and never print.
impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("File")
            .field("name", &self.name)
            .field("bytes", &self.contents.len())
            .finish()
    }
}

/// Annotates each chosen frame with its ink and click ripple, lays them out on a contact sheet,
/// and badges every image with its keyframe's number and time.
///
/// `frames` holds the full-resolution image for each keyframe, in keyframe order.
///
/// # Errors
///
/// Fails when `frames` does not hold exactly one image per keyframe.
pub fn compose(
    recording: Recording,
    selection: Selection,
    frames: Vec<Image>,
    typeface: &Typeface,
) -> Result<Bundle, Error> {
    let Selection { marks, keyframes } = selection;
    if frames.len() != keyframes.len() {
        return Err(Error::frames(keyframes.len(), frames.len()));
    }
    let annotated: Vec<(Image, Corner)> = frames
        .into_iter()
        .zip(&keyframes)
        .map(|(mut image, keyframe)| {
            let corner = draw::annotate(&mut image, keyframe, &recording, shown(&marks, keyframe));
            (image, corner)
        })
        .collect();
    let contact_sheet = sheet::render(&keyframes, &annotated, typeface)?;
    let frames = annotated
        .into_iter()
        .zip(&keyframes)
        .map(|((mut image, corner), keyframe)| {
            let (width, height) = (image.width() as f32, image.height() as f32);
            let whole = Rect::from_origin(Point::default(), width, height);
            let scale = draw::badge_scale(width, height);
            draw::badge(
                &mut image.as_mut(),
                whole,
                keyframe,
                scale,
                corner,
                typeface,
            );
            image
        })
        .collect();
    Ok(Bundle {
        manifest: Manifest {
            version: Manifest::VERSION,
            recording,
            marks,
            keyframes,
        },
        frames,
        contact_sheet,
    })
}

/// The marks drawn on a keyframe.
fn shown<'a>(marks: &'a [Mark], keyframe: &Keyframe) -> &'a [Mark] {
    marks.get(keyframe.marks.clone()).unwrap_or_default()
}

impl Bundle {
    /// What was recorded and chosen.
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The summary an agent reads first, naming files under `directory`, where the bundle will be
    /// written.
    ///
    /// One line per keyframe gives its number and time, what the pointer or ink did there, and
    /// what was said, or only what happened when nothing was said. Window titles and other screen
    /// text never appear in it.
    #[must_use]
    pub fn summary(&self, directory: &Path) -> String {
        let Manifest {
            recording,
            marks,
            keyframes,
            ..
        } = &self.manifest;
        let mut lines = vec![format!(
            "Screen recording ({}, {}): {}",
            recording.duration,
            Count(keyframes.len(), "keyframe"),
            directory.display()
        )];
        lines.push(format!(
            "Contact sheet: {}",
            directory.join(CONTACT_SHEET).display()
        ));
        lines.extend(
            keyframes
                .iter()
                .map(|keyframe| match keyframe.said.as_deref() {
                    Some(said) => format!(
                        "- #{} ({}){} “{said}”",
                        keyframe.index,
                        keyframe.at,
                        Marker(keyframe, shown(marks, keyframe))
                    ),
                    None => format!("- #{} ({}) {}", keyframe.index, keyframe.at, keyframe.event),
                }),
        );
        match keyframes.as_slice() {
            [] => {},
            [only] => lines.push(format!(
                "Full-size keyframe: {}. Timeline: {MANIFEST}.",
                only.file()
            )),
            [first, .., last] => lines.push(format!(
                "Full-size keyframes: {} to {}. Timeline: {MANIFEST}.",
                first.file(),
                last.file()
            )),
        }
        lines.push(String::new());
        lines.join("\n")
    }

    /// Every file of the bundle, encoding images on up to one thread per core.
    ///
    /// # Errors
    ///
    /// Fails if an image or the manifest cannot be encoded, which valid input never causes.
    pub fn files(&self, directory: &Path) -> Result<Vec<File>, Error> {
        let images: Vec<(String, &Image)> =
            std::iter::once((CONTACT_SHEET.to_owned(), &self.contact_sheet))
                .chain(
                    self.manifest
                        .keyframes
                        .iter()
                        .map(Keyframe::file)
                        .zip(&self.frames),
                )
                .collect();
        let mut files = parallel::map(&images, |(name, image)| {
            image.encode_png().map(|contents| File {
                name: name.clone(),
                contents,
            })
        })
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
        files.push(File {
            name: SUMMARY.to_owned(),
            contents: self.summary(directory).into_bytes(),
        });
        files.push(File {
            name: MANIFEST.to_owned(),
            contents: serde_json::to_vec_pretty(&self.manifest).map_err(Error::manifest)?,
        });
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Action, InkPoint, Moment, Narration, Reason, Shape, Size, Span, Stroke, Word};

    fn at(millis: u64) -> Moment {
        Moment::from_millis(millis)
    }

    #[test]
    fn show_json_keeps_its_wire_format() {
        let click = Action::Click {
            at: at(400),
            point: Point::new(0.5, 0.25),
        };
        let word = |start: u64, text: &str| Word {
            span: Span::new(at(start), at(start + 200)),
            text: text.to_owned(),
        };
        let ink = |x: f32, millis: u64| InkPoint {
            point: Point::new(x, 0.5),
            at: at(millis),
        };
        let manifest = Manifest {
            version: Manifest::VERSION,
            recording: Recording {
                duration: at(2_000),
                surface: Size {
                    width: 800.0,
                    height: 600.0,
                },
                strokes: vec![Stroke::new(vec![ink(0.25, 100), ink(0.75, 300)]).unwrap()],
                actions: vec![
                    click,
                    Action::Drag {
                        span: Span::new(at(600), at(900)),
                        from: Point::new(0.25, 0.75),
                        to: Point::new(0.75, 0.75),
                    },
                ],
                narration: Narration::Words(vec![word(100, "Look"), word(300, "here.")]),
            },
            marks: vec![Mark {
                shape: Shape::Line {
                    from: Point::new(0.25, 0.5),
                    to: Point::new(0.75, 0.5),
                },
                strokes: 0..1,
                span: Span::new(at(100), at(300)),
                bounds: Rect {
                    left: 0.25,
                    top: 0.5,
                    right: 0.75,
                    bottom: 0.5,
                },
            }],
            keyframes: vec![
                Keyframe {
                    index: 1,
                    at: at(300),
                    reason: Reason::Mark,
                    event: "drew a line".to_owned(),
                    said: Some("Look here.".to_owned()),
                    marks: 0..1,
                    action: None,
                },
                Keyframe {
                    index: 2,
                    at: at(1_000),
                    reason: Reason::Click,
                    event: "clicked".to_owned(),
                    said: None,
                    marks: 0..0,
                    action: Some(click),
                },
                Keyframe {
                    index: 3,
                    at: at(1_500),
                    reason: Reason::SceneChange,
                    event: "scene changed".to_owned(),
                    said: None,
                    marks: 0..0,
                    action: None,
                },
            ],
        };
        let click = json!({"kind": "click", "at": 400, "point": [0.5, 0.25]});
        let golden = json!({
            "version": 1,
            "duration": 2000,
            "surface": {"width": 800.0, "height": 600.0},
            "strokes": [[[0.25, 0.5, 100], [0.75, 0.5, 300]]],
            "actions": [
                click,
                {"kind": "drag", "span": [600, 900], "from": [0.25, 0.75], "to": [0.75, 0.75]},
            ],
            "narration": {"words": [
                {"span": [100, 300], "text": "Look"},
                {"span": [300, 500], "text": "here."},
            ]},
            "marks": [{
                "shape": "line",
                "from": [0.25, 0.5],
                "to": [0.75, 0.5],
                "strokes": [0, 1],
                "span": [100, 300],
                "bounds": [0.25, 0.5, 0.75, 0.5],
            }],
            "keyframes": [
                {
                    "index": 1,
                    "at": 300,
                    "reason": "mark",
                    "event": "drew a line",
                    "said": "Look here.",
                    "marks": [0, 1],
                },
                {
                    "index": 2,
                    "at": 1000,
                    "reason": "click",
                    "event": "clicked",
                    "marks": [0, 0],
                    "action": click,
                },
                {"index": 3, "at": 1500, "reason": "sceneChange", "event": "scene changed", "marks": [0, 0]},
            ],
        });
        let changed =
            "show.json changed; bump Manifest::VERSION if an existing reader would misread it";
        assert_eq!(
            serde_json::to_value(&manifest).unwrap(),
            golden,
            "{changed}"
        );
        assert_eq!(
            serde_json::from_value::<Manifest>(golden).unwrap(),
            manifest,
            "{changed}"
        );
    }

    #[test]
    fn debugging_a_file_never_prints_its_contents() {
        let file = File {
            name: "frames/frame-01.png".to_owned(),
            contents: vec![0x5A; 4_096],
        };
        let shown = format!("{file:?}");
        assert!(shown.contains("frame-01") && shown.len() < 80, "{shown}");
    }
}
