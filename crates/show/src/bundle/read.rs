//! Reading the manifest without trusting its indexes or exposing narration in errors.

use std::fmt;

use serde::Deserialize;

use super::{Bundle, Manifest, sheet_file};
use crate::{Keyframe, Mark, Reason, Recording, select::MOST_KEYFRAMES, sheet};

/// Why a manifest cannot be inspected. No variant or error source contains its private text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unreadable {
    /// This reader does not understand the declared format version.
    Unsupported(u32),
    /// JSON or a field's type is invalid at this one-based position.
    Malformed {
        /// Line containing the failure.
        line: usize,
        /// Column containing the failure.
        column: usize,
    },
    /// The manifest's references or ordering violate this fixed, content-free rule.
    Inconsistent(&'static str),
}

impl fmt::Display for Unreadable {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(version) => write!(out, "unsupported Show format version {version}"),
            Self::Malformed { line, column } => {
                write!(
                    out,
                    "show.json is malformed at line {line}, column {column}"
                )
            },
            Self::Inconsistent(rule) => write!(out, "show.json is inconsistent: {rule}"),
        }
    }
}

impl std::error::Error for Unreadable {}

/// Only the decoder can create unchecked fields; `Manifest` exposes them read-only after
/// validation. Unknown fields remain compatible with additive changes to the format.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Decoded {
    version: u32,
    #[serde(flatten)]
    recording: Recording,
    marks: Vec<Mark>,
    keyframes: Vec<Keyframe>,
}

fn malformed(error: serde_json::Error) -> Unreadable {
    Unreadable::Malformed {
        line: error.line(),
        column: error.column(),
    }
}

impl Manifest {
    /// Parses format 2 and validates frame numbers, times, and mark and stroke references.
    /// Unknown fields are ignored. No file is read and no image is decoded.
    ///
    /// # Errors
    /// Returns [`Unreadable`] for an unsupported version, invalid JSON, or inconsistent
    /// references. Errors never retain the JSON or a decoder message that could quote it.
    pub fn parse(bytes: &[u8]) -> Result<Self, Unreadable> {
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        let version: Version = serde_json::from_slice(bytes).map_err(malformed)?;
        if version.version != Self::VERSION {
            return Err(Unreadable::Unsupported(version.version));
        }
        let Decoded {
            version,
            recording,
            marks,
            keyframes,
        } = serde_json::from_slice(bytes).map_err(malformed)?;
        let manifest = Self {
            version,
            recording,
            marks,
            keyframes,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Narration, actions, capture dimensions, and how the recording ended.
    #[must_use]
    pub const fn recording(&self) -> &Recording {
        &self.recording
    }

    /// Saved keyframes, numbered consecutively from one and in timeline order.
    #[must_use]
    pub fn keyframes(&self) -> &[Keyframe] {
        &self.keyframes
    }

    /// Marks numbered consecutively from one. A mark need not have a saved keyframe.
    #[must_use]
    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    /// Each contact sheet's relative filename and the keyframes it shows, in reading order.
    pub fn sheets(&self) -> impl Iterator<Item = (String, &[Keyframe])> {
        sheet::pages(&self.keyframes)
            .enumerate()
            .map(|(index, frames)| (sheet_file(index), frames))
    }

    /// Every expected relative filename. Names derive from the format and validated numbers,
    /// never from paths supplied by the manifest.
    pub fn files(&self) -> impl Iterator<Item = String> {
        [Self::FILE.to_owned(), Bundle::SUMMARY.to_owned()]
            .into_iter()
            .chain(self.sheets().map(|(name, _)| name))
            .chain(self.keyframes.iter().map(Keyframe::file))
    }

    fn validate(&self) -> Result<(), Unreadable> {
        let require = |holds, rule| {
            if holds {
                Ok(())
            } else {
                Err(Unreadable::Inconsistent(rule))
            }
        };
        require(
            self.recording
                .spaces
                .valid(self.recording.strokes.len(), self.recording.pins.len()),
            "pointer coordinate spaces are invalid",
        )?;
        require(
            (1..=MOST_KEYFRAMES).contains(&self.keyframes.len()),
            "a Show needs between 1 and 36 keyframes",
        )?;
        let mut previous_time = crate::Moment::START;
        let mut previous_mark = 0;
        for (position, frame) in self.keyframes.iter().enumerate() {
            require(
                frame.index == position.saturating_add(1),
                "frame numbers must start at 1 and be consecutive",
            )?;
            require(
                frame.at >= previous_time && frame.at <= self.recording.duration,
                "frame times must be ordered and within the recording",
            )?;
            require(
                frame.marks.end <= self.marks.len(),
                "a frame refers to a mark that does not exist",
            )?;
            if !frame.marks.is_empty() {
                require(
                    frame.reason == Reason::Mark,
                    "only mark keyframes may refer to marks",
                )?;
                require(
                    frame.marks.start >= previous_mark,
                    "frame mark ranges must be ordered and disjoint",
                )?;
                previous_mark = frame.marks.end;
            }
            previous_time = frame.at;
        }
        for (position, mark) in self.marks.iter().enumerate() {
            require(
                mark.number == position.saturating_add(1),
                "mark numbers must start at 1 and be consecutive",
            )?;
            require(
                mark.strokes.end <= self.recording.strokes.len(),
                "a mark refers to a stroke that does not exist",
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
