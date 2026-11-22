use std::{
    fmt,
    path::{Path, PathBuf},
};

use serde::Serialize;

mod read;
pub use read::Unreadable;

use crate::{
    Ending, Error, Image, Keyframe, Mark, Moment, Narration, NarrationLoss, PixelRect, Recording,
    Rect, Selection, Typeface, Wsl,
    describe::{Count, Marker},
    draw::{self, Corner},
    framing, parallel,
    select::Shot,
    sheet::{self, Tile},
};

const CONTACT_SHEET: &str = "contact-sheet.png";

fn sheet_file(number: usize) -> String {
    if number == 0 {
        CONTACT_SHEET.to_owned()
    } else {
        format!("contact-sheet-{:02}.png", number.saturating_add(1))
    }
}

impl Keyframe {
    #[must_use]
    pub fn file(&self) -> String {
        format!("frames/frame-{:02}.png", self.index)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub(crate) version: u32,
    #[serde(flatten)]
    pub(crate) recording: Recording,
    pub(crate) marks: Vec<Mark>,
    pub(crate) keyframes: Vec<Keyframe>,
}

impl Manifest {
    pub const FILE: &str = "show.json";
    /// Changes only when a reader must change to read the manifest.
    pub(crate) const VERSION: u32 = 3;
}

#[derive(Clone, Debug)]
pub struct Bundle {
    pub(crate) manifest: Manifest,
    images: Vec<File>,
}

#[derive(Clone, Copy, Debug)]
pub struct Location<'a> {
    directory: &'a Path,
    id: Option<&'a str>,
    wsl: Option<&'a Wsl>,
}

impl<'a> Location<'a> {
    #[must_use]
    pub fn managed(directory: &'a Path, wsl: Option<&'a Wsl>) -> Option<Self> {
        let id = directory.file_name()?.to_str()?;
        Some(Self {
            directory,
            id: Some(id),
            wsl,
        })
    }

    #[must_use]
    pub const fn unmanaged(directory: &'a Path, wsl: Option<&'a Wsl>) -> Self {
        Self {
            directory,
            id: None,
            wsl,
        }
    }

    #[must_use]
    pub const fn directory(&self) -> &'a Path {
        self.directory
    }

    #[must_use]
    pub const fn id(&self) -> Option<&'a str> {
        self.id
    }

    #[must_use]
    pub fn in_wsl(&self) -> Option<String> {
        self.wsl?.path(self.directory)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub contents: Vec<u8>,
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("File")
            .field("name", &self.name)
            .field("bytes", &self.contents.len())
            .finish()
    }
}

/// Annotates, lays out, badges, and encodes the selection's keyframes, one sheet at a time.
///
/// `frame` may run on several threads at once. Loading per sheet keeps only one sheet's frames
/// decoded together.
pub fn compose<E: From<Error> + Send>(
    recording: Recording,
    selection: Selection,
    frame: impl Fn(Moment) -> Result<Image, E> + Sync,
    typeface: &Typeface,
) -> Result<Bundle, E> {
    let Selection { marks, shots } = selection;
    let mut sheets = Vec::new();
    let mut frames = Vec::with_capacity(shots.len());
    let mut crops = Vec::with_capacity(shots.len());
    for (number, page) in sheet::pages(&shots).enumerate() {
        let (annotated, cropped): (Vec<(Image, Corner)>, Vec<Option<Rect>>) =
            parallel::map(page, |Shot { keyframe, plan }| {
                let mut image = frame(keyframe.at)?;
                let drawn = draw::annotate(&mut image, keyframe, &recording, &marks, typeface);
                let crop = plan
                    .and_then(|plan| framing::fit(plan, drawn.extent(), &image, recording.surface))
                    .map(|crop| PixelRect::covering(crop, image.width(), image.height()))
                    .filter(|pixels| {
                        pixels.width < image.width() || pixels.height < image.height()
                    });
                Ok(match crop {
                    Some(pixels) => {
                        let part = pixels.part(image.width(), image.height());
                        ((image.cut(pixels)?, drawn.corner(part)), Some(part))
                    },
                    None => ((image, drawn.corner(Rect::UNIT)), None),
                })
            })
            .into_iter()
            .collect::<Result<_, E>>()?;
        crops.extend(cropped);
        sheets.push(File {
            name: sheet_file(number),
            contents: sheet::render(
                &page
                    .iter()
                    .zip(&annotated)
                    .map(|(Shot { keyframe, .. }, (image, corner))| Tile {
                        image,
                        badge: keyframe.into(),
                        corner: *corner,
                    })
                    .collect::<Vec<_>>(),
                typeface,
            )?
            .encode_png()?,
        });
        let badged: Vec<(&Keyframe, Image)> = page
            .iter()
            .zip(annotated)
            .map(|(Shot { keyframe, .. }, (mut image, corner))| {
                draw::badge_frame(&mut image, keyframe, corner, typeface);
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
    let keyframes = shots
        .into_iter()
        .zip(crops)
        .map(|(Shot { keyframe, .. }, crop)| Keyframe { crop, ..keyframe })
        .collect();
    Ok(Bundle {
        manifest: Manifest {
            version: Manifest::VERSION,
            recording,
            marks,
            keyframes,
        },
        images: sheets.into_iter().chain(frames).collect(),
    })
}

struct Summary<'a> {
    manifest: &'a Manifest,
    location: Location<'a>,
    command: Option<&'a Path>,
}

impl fmt::Display for Summary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            manifest,
            location,
            command,
        } = *self;
        let Location { directory, id, .. } = location;
        let Manifest {
            recording,
            marks,
            keyframes,
            ..
        } = manifest;

        // Joined part by part, a bundle file's `/`-separated name takes the platform separator.
        let file = |name: &str| {
            let within: PathBuf = name.split('/').collect();
            Code(directory.join(within).display().to_string())
        };
        writeln!(
            f,
            "Screen recording ({}, {}): {}",
            recording.duration,
            Count(keyframes.len(), "keyframe"),
            Code(directory.display())
        )?;
        let stopped = match recording.ending {
            Ending::Finished => None,
            Ending::ScreenStopped => Some("The screen stopped recording"),
            Ending::WindowClosed => Some("The recorded window closed"),
            Ending::WindowOffScreen => Some("The recorded window went off screen"),
        };
        if let Some(stopped) = stopped {
            writeln!(
                f,
                "{stopped} at {}, before the user finished the Show; later steps are missing.",
                recording.duration
            )?;
        }
        match recording.narration_lost {
            Some(NarrationLoss::Incomplete) => writeln!(
                f,
                "Narration is incomplete: what the user said after the audio stopped is missing."
            )?,
            Some(NarrationLoss::AudioStopped) if stopped.is_some() => writeln!(
                f,
                "Audio capture stopped as well; what the user said is missing."
            )?,
            Some(NarrationLoss::AudioStopped) => writeln!(
                f,
                "Audio capture stopped at {} and the Show ended there; what the user said and any \
                 later steps are missing.",
                recording.duration
            )?,
            Some(NarrationLoss::AudioSilent) => writeln!(
                f,
                "The microphone sent only silence; what the user said is missing."
            )?,
            Some(NarrationLoss::NotTranscribed) => writeln!(
                f,
                "The user's speech could not be transcribed; what the user said is missing."
            )?,
            None => {},
        }
        if let Some(wsl) = location.in_wsl() {
            writeln!(f, "From WSL, the same files are under {}.", Code(wsl))?;
        }
        let pages: Vec<&[Keyframe]> = sheet::pages(keyframes).collect();
        if !pages.is_empty() {
            let sheets = if pages.len() == 1 { "sheet" } else { "sheets" };
            writeln!(
                f,
                "Before acting, open the contact {sheets}, then the full-size frames that matter: \
                 they show what the words leave out. Speech recognition can mishear a name; where \
                 the screen spells it differently, trust the screen. Cite frames as #N and marks \
                 by their circled numbers."
            )?;
        }
        for (number, page) in pages.iter().enumerate() {
            let sheet = file(&sheet_file(number));
            match (pages.len(), page.first(), page.last()) {
                (2.., Some(first), Some(last)) => writeln!(
                    f,
                    "Contact sheet {} (frames #{}–#{}): {sheet}",
                    number.saturating_add(1),
                    first.index,
                    last.index,
                ),
                _ => writeln!(f, "Contact sheet: {sheet}"),
            }?;
        }
        for keyframe in keyframes {
            let Keyframe {
                index,
                at,
                event,
                said,
                ..
            } = keyframe;
            match said {
                Some(said) => writeln!(
                    f,
                    "- #{index} ({at}){} “{said}”",
                    Marker(keyframe, keyframe.marks_in(marks))
                ),
                None => writeln!(f, "- #{index} ({at}) {event}"),
            }?;
        }
        if let Narration::Text(untimed) = &recording.narration
            && !untimed.trim().is_empty()
        {
            writeln!(
                f,
                "Untimed narration (the speech engine gave no word times, so no keyframe owns \
                 it):\n“{}”",
                untimed.trim()
            )?;
        }
        let timeline = file(Manifest::FILE);
        match keyframes.as_slice() {
            [] => Ok(()),
            [only] => writeln!(
                f,
                "Full-size keyframe: {}. Timeline: {timeline}.",
                file(&only.file())
            ),
            [first, .., last] => writeln!(
                f,
                "Full-size keyframes: {} to {}. Timeline: {timeline}.",
                file(&first.file()),
                file(&last.file())
            ),
        }?;
        let cropped: Vec<usize> = keyframes
            .iter()
            .filter(|keyframe| keyframe.crop.is_some())
            .map(|keyframe| keyframe.index)
            .collect();
        match cropped.as_slice() {
            [] => {},
            [only] => writeln!(
                f,
                "Frame #{only} is cropped to the area the user worked in; the timeline gives the \
                 crop as a fraction of the screen."
            )?,
            _ if cropped.len() == keyframes.len() => writeln!(
                f,
                "Every keyframe is cropped to the area the user worked in; the timeline gives \
                 each crop as a fraction of the screen."
            )?,
            several => writeln!(
                f,
                "Frames {} are cropped to the area the user worked in; the timeline gives each \
                 crop as a fraction of the screen.",
                Frames(several)
            )?,
        }
        let Some(id) = id else {
            return Ok(());
        };
        writeln!(
            f,
            "Show ID: {}. An agent with Speakeasy's MCP tools can open this Show by that ID.",
            Code(id)
        )?;
        let Some(command) = command else {
            return Ok(());
        };
        writeln!(
            f,
            "Screen samples between keyframes are retained. Local command: {} (a path, not shell \
             syntax; quote it for your shell).",
            Code(command.display())
        )?;
        writeln!(
            f,
            "Use {inspect} to check this Show and {frames} for sample times; add --at M:SS.mmm, \
             --phrase TEXT, or --mark N to focus. {skill} explains image exports and sampling \
             gaps. If the ID does not resolve, pass the folder above as one SHOW argument. If the \
             command has moved, read the bundle files directly.",
            inspect = Code(format!("show inspect {id} --json")),
            frames = Code(format!("show frames {id} --json")),
            skill = Code("show skill"),
        )?;
        if cropped.is_empty() {
            return Ok(());
        }
        writeln!(
            f,
            "For the whole screen behind a cropped frame, run {} with that frame's time.",
            Code(format!(
                "show frame {id} --at M:SS.mmm --out NEW.png --json"
            ))
        )
    }
}

struct Frames<'a>(&'a [usize]);

impl fmt::Display for Frames<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            [] => Ok(()),
            [only] => write!(f, "#{only}"),
            [first, last] => write!(f, "#{first} and #{last}"),
            [rest @ .., last] => {
                for index in rest {
                    write!(f, "#{index}, ")?;
                }
                write!(f, "and #{last}")
            },
        }
    }
}

/// Text as a Markdown code span, so a reader can tell where a path with spaces ends.
///
/// Markdown's rule: the fence is one backtick longer than any run inside, and text starting or
/// ending with a backtick or space is padded with a space that reading strips.
struct Code<T>(T);

impl<T: fmt::Display> fmt::Display for Code<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.0.to_string();
        let longest_run = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
        let fence = "`".repeat(longest_run.saturating_add(1));
        let edges = ['`', ' '];
        let pad = if text.starts_with(edges) || text.ends_with(edges) {
            " "
        } else {
            ""
        };
        write!(f, "{fence}{pad}{text}{pad}{fence}")
    }
}

impl Bundle {
    pub const SUMMARY: &str = "summary.md";

    #[must_use]
    pub fn ended(mut self, ending: Ending, narration_lost: Option<NarrationLoss>) -> Self {
        self.manifest.recording.ending = ending;
        self.manifest.recording.narration_lost = narration_lost;
        self
    }

    pub fn into_files(
        self,
        location: Location<'_>,
        command: Option<&Path>,
    ) -> Result<(String, Vec<File>), Error> {
        let summary = Summary {
            manifest: &self.manifest,
            location,
            command,
        }
        .to_string();
        let mut files = self.images;
        files.push(File {
            name: Self::SUMMARY.to_owned(),
            contents: summary.clone().into_bytes(),
        });
        files.push(File {
            name: Manifest::FILE.to_owned(),
            contents: serde_json::to_vec_pretty(&self.manifest).map_err(Error::manifest)?,
        });
        Ok((summary, files))
    }
}

#[cfg(test)]
mod tests;
