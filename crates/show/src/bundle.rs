//! The finished Show: annotated keyframes, the contact sheet, the summary, and the manifest.

use std::{fmt, ops::Range, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    Error, Image, Keyframe, Mark, Point, Recording, Rect, Selection, Typeface,
    draw::{self, Corner},
    parallel,
    select::{Reason, Verb},
    sheet,
};

const CONTACT_SHEET: &str = "contact-sheet.png";
const SUMMARY: &str = "summary.md";
const MANIFEST: &str = "show.json";

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
    /// Absent when nothing was chosen to show.
    contact_sheet: Option<Image>,
}

/// One file of a bundle, named by its path within the bundle's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// The path within the bundle, with `/` separators.
    pub name: String,
    /// The file's bytes.
    pub contents: Vec<u8>,
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
            let strokes: Vec<Range<usize>> = shown(&marks, keyframe)
                .iter()
                .map(|mark| mark.strokes.clone())
                .collect();
            let corner = draw::annotate(&mut image, keyframe, &recording, &strokes);
            (image, corner)
        })
        .collect();
    let contact_sheet = if annotated.is_empty() {
        None
    } else {
        Some(sheet::render(&keyframes, &annotated, typeface)?)
    };
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
        if self.contact_sheet.is_some() {
            lines.push(format!(
                "Contact sheet: {}",
                directory.join(CONTACT_SHEET).display()
            ));
        }
        lines.extend(keyframes.iter().map(|keyframe| {
            let said = keyframe
                .said
                .as_deref()
                .map(str::trim)
                .filter(|said| !said.is_empty());
            match said {
                Some(said) => format!(
                    "- #{} ({}){} “{said}”",
                    keyframe.index,
                    keyframe.at,
                    Marker(keyframe, shown(marks, keyframe))
                ),
                None => format!("- #{} ({}) {}", keyframe.index, keyframe.at, keyframe.event),
            }
        }));
        let text = recording.narration.text();
        if keyframes.is_empty() && !text.is_empty() {
            lines.push(format!("“{text}”"));
        }
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
        let images: Vec<(String, &Image)> = self
            .contact_sheet
            .iter()
            .map(|sheet| (CONTACT_SHEET.to_owned(), sheet))
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

/// What a keyframe shows happening beside the words said over it: the click or drag whose result
/// it shows, or what its ink does.
struct Marker<'a>(&'a Keyframe, &'a [Mark]);

impl fmt::Display for Marker<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(keyframe, marks) = self;
        match (keyframe.reason, keyframe.action) {
            (Reason::Click, Some(action)) => write!(f, " [{}]", Verb(action)),
            (Reason::Mark, _) if !marks.is_empty() => {
                let mut verbs: Vec<&str> = Vec::with_capacity(marks.len());
                for verb in marks.iter().map(|mark| mark.shape.verb()) {
                    if !verbs.contains(&verb) {
                        verbs.push(verb);
                    }
                }
                write!(f, " [{}]", verbs.join(", "))
            },
            _ => Ok(()),
        }
    }
}

/// A count and its noun, plural unless the count is one.
struct Count(usize, &'static str);

impl fmt::Display for Count {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(count, noun) = *self;
        let plural = if count == 1 { "" } else { "s" };
        write!(f, "{count} {noun}{plural}")
    }
}
