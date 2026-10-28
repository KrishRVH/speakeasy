//! The finished Show: annotated keyframes, contact sheets, the summary, and the manifest.

use std::{fmt, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    Error, Image, Keyframe, Mark, Moment, Point, Recording, Rect, Selection, Typeface,
    describe::{Count, Marker},
    draw::{self, Corner},
    parallel, sheet,
};

const CONTACT_SHEET: &str = "contact-sheet.png";
const SUMMARY: &str = "summary.md";
const MANIFEST: &str = "show.json";

/// The file for contact sheet `page`, from zero: `contact-sheet.png`, then `contact-sheet-02.png`
/// onward.
fn sheet_file(page: usize) -> String {
    if page == 0 {
        CONTACT_SHEET.to_owned()
    } else {
        format!("contact-sheet-{:02}.png", page.saturating_add(1))
    }
}

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
    pub const VERSION: u32 = 2;
}

/// A Show ready to write: its manifest and its images, annotated and badged.
#[derive(Clone, Debug)]
pub struct Bundle {
    pub(crate) manifest: Manifest,
    images: Vec<File>,
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

/// Annotates each chosen frame with its ink, numbered labels, and click ripple, lays them out on
/// contact sheets, and badges every image with its keyframe's number and time.
///
/// `frame` supplies the full-resolution image on screen at a keyframe's moment, and may be called
/// on several threads at once. Frames load, compose, and encode one sheet at a time, so only one
/// sheet's frames are ever decoded together.
///
/// # Errors
///
/// Fails with the first error `frame` returns, before loading the next sheet's frames, or when an
/// image cannot be built or encoded.
pub fn compose<E: From<Error> + Send>(
    recording: Recording,
    selection: Selection,
    frame: impl Fn(Moment) -> Result<Image, E> + Sync,
    typeface: &Typeface,
) -> Result<Bundle, E> {
    let Selection { marks, keyframes } = selection;
    let mut sheets = Vec::new();
    let mut frames = Vec::with_capacity(keyframes.len());
    for (page, keyframes) in sheet::pages(&keyframes).enumerate() {
        let annotated = parallel::map(keyframes, |keyframe| {
            let mut image = frame(keyframe.at)?;
            let corner = draw::annotate(&mut image, keyframe, &recording, &marks, typeface);
            Ok((image, corner))
        })
        .into_iter()
        .collect::<Result<Vec<(Image, Corner)>, E>>()?;
        sheets.push(File {
            name: sheet_file(page),
            contents: sheet::render(keyframes, &annotated, typeface)?.encode_png()?,
        });
        let badged: Vec<(&Keyframe, Image)> = keyframes
            .iter()
            .zip(annotated)
            .map(|(keyframe, (mut image, corner))| {
                let (width, height) = (image.width() as f32, image.height() as f32);
                let whole = Rect::from_origin(Point::default(), width, height);
                draw::badge(
                    &mut image.as_mut(),
                    whole,
                    keyframe,
                    draw::badge_scale(width, height),
                    corner,
                    typeface,
                );
                (keyframe, image)
            })
            .collect();
        let encoded = parallel::map(&badged, |(keyframe, image)| {
            image.encode_png().map(|contents| File {
                name: keyframe.file(),
                contents,
            })
        });
        for file in encoded {
            frames.push(file?);
        }
    }
    sheets.append(&mut frames);
    Ok(Bundle {
        manifest: Manifest {
            version: Manifest::VERSION,
            recording,
            marks,
            keyframes,
        },
        images: sheets,
    })
}

/// The marks drawn on a keyframe.
fn shown<'a>(marks: &'a [Mark], keyframe: &Keyframe) -> &'a [Mark] {
    marks.get(keyframe.marks.clone()).unwrap_or_default()
}

impl Bundle {
    /// The summary an agent reads first, naming files under `directory`, where the bundle will be
    /// written.
    ///
    /// One line per keyframe gives its number and time, what the pointer or ink did there, and
    /// what was said, or only what happened when nothing was said. Window titles and other screen
    /// text never appear in it.
    fn summary(&self, directory: &Path) -> String {
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
        let pages: Vec<&[Keyframe]> = sheet::pages(keyframes).collect();
        for (page, shown) in pages.iter().enumerate() {
            let path = directory.join(sheet_file(page));
            lines.push(match (pages.len(), shown.first(), shown.last()) {
                (2.., Some(first), Some(last)) => format!(
                    "Contact sheet {} (frames #{}–#{}): {}",
                    page.saturating_add(1),
                    first.index,
                    last.index,
                    path.display()
                ),
                _ => format!("Contact sheet: {}", path.display()),
            });
        }
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

    /// Every file of the bundle for `directory`, where it will be written, with already encoded
    /// images, and the summary among them, which names the files there and which an agent reads
    /// first.
    ///
    /// # Errors
    ///
    /// Fails if the manifest cannot be encoded, which valid input never causes.
    pub fn into_files(self, directory: &Path) -> Result<(String, Vec<File>), Error> {
        let summary = self.summary(directory);
        let mut files = self.images;
        files.push(File {
            name: SUMMARY.to_owned(),
            contents: summary.clone().into_bytes(),
        });
        files.push(File {
            name: MANIFEST.to_owned(),
            contents: serde_json::to_vec_pretty(&self.manifest).map_err(Error::manifest)?,
        });
        Ok((summary, files))
    }
}

#[cfg(test)]
mod tests;
