use std::fmt;

use serde::Deserialize;

use super::{Bundle, Manifest, sheet_file};
use crate::{Keyframe, Mark, Reason, Recording, select::MOST_KEYFRAMES, sheet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unreadable {
    Unsupported(u32),
    Malformed { line: usize, column: usize },
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

    #[must_use]
    pub const fn recording(&self) -> &Recording {
        &self.recording
    }

    #[must_use]
    pub fn keyframes(&self) -> &[Keyframe] {
        &self.keyframes
    }

    #[must_use]
    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    pub fn sheets(&self) -> impl Iterator<Item = (String, &[Keyframe])> {
        sheet::pages(&self.keyframes)
            .enumerate()
            .map(|(index, frames)| (sheet_file(index), frames))
    }

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
            self.recording.spaces.valid(
                self.recording.strokes.len(),
                self.recording.pins.len(),
                self.recording.actions.len(),
                self.recording.duration,
            ),
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
