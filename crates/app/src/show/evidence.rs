pub(super) mod query;

use std::{
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::Serialize;
use speakeasy_show::{Bundle, Ending, Manifest, NarrationLoss, Reason, Rect};

use super::store::{
    address, checkpoint,
    recall::{Entry, FileProblem, Health, ReadError, ReadShow},
    retention::Retention,
};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Status {
    Ok,
    Damaged,
    Unsupported,
    Interrupted,
    NarrationPending,
}

impl From<Health> for Status {
    fn from(health: Health) -> Self {
        match health {
            Health::Ok => Self::Ok,
            Health::Damaged => Self::Damaged,
            Health::Unsupported => Self::Unsupported,
        }
    }
}

impl Status {
    pub(super) const fn text(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Damaged => "damaged",
            Self::Unsupported => "unsupported",
            Self::Interrupted => "interrupted",
            Self::NarrationPending => "narration pending",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Overview {
    pub(super) id: Option<String>,
    pub(super) directory: PathBuf,
    pub(super) status: Status,
    pub(super) pinned: Option<bool>,
    pub(super) expires_at: Option<String>,
    pub(super) duration_ms: Option<u64>,
    pub(super) keyframes: Option<usize>,
    pub(super) marks: Option<usize>,
    pub(super) problems: Vec<Problem>,
}

#[derive(Serialize)]
pub(super) struct Problem {
    pub(super) file: PathBuf,
    pub(super) reason: String,
}

impl Overview {
    pub(super) fn of(entry: &Entry, parent: Option<&Path>) -> Self {
        let file = |problem: &FileProblem| Problem {
            file: entry.file(&problem.name),
            reason: problem.reason.clone(),
        };
        let problems = match &entry.show {
            Ok(show) => show.problems.iter().map(file).collect(),
            Err(ReadError::File(problem)) => vec![file(problem)],
            Err(error @ ReadError::Manifest(_)) => vec![Problem {
                file: entry.file(Manifest::FILE),
                reason: error.to_string(),
            }],
        };
        let show = entry.show.as_ref().ok();

        let id =
            parent.and_then(|parent| address::managed_id(parent, &entry.directory).ok().flatten());
        let retention = id
            .as_ref()
            .and_then(|_| Retention::read(&entry.directory).ok());
        Self {
            id: id.map(|id| id.as_str().to_owned()),
            directory: entry.directory.clone(),
            status: entry.health().into(),
            pinned: show.map(|show| show.pinned),
            expires_at: retention.and_then(Retention::expires_at),
            duration_ms: show.map(|show| show.manifest.recording().duration.as_millis()),
            keyframes: show.map(|show| show.manifest.keyframes().len()),
            marks: show.map(|show| show.manifest.marks().len()),
            problems,
        }
    }
}

#[derive(Serialize)]
pub(super) struct Inspection {
    #[serde(flatten)]
    pub(super) overview: Overview,
    #[serde(flatten)]
    pub(super) detail: Option<Detail>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Detail {
    pub(super) ending: Ending,
    pub(super) narration_lost: Option<NarrationLoss>,
    pub(super) summary: PathBuf,
    pub(super) manifest: PathBuf,
    pub(super) sheets: Vec<Sheet>,
    pub(super) frames: Vec<Frame>,
    pub(super) mark_frames: Vec<MarkFrame>,
    pub(super) movie: Option<Movie>,
}

#[derive(Serialize)]
pub(super) struct Movie {
    pub(super) path: PathBuf,
    #[serde(flatten)]
    pub(super) timing: super::store::movie::Metadata,
}

#[derive(Serialize)]
pub(super) struct Sheet {
    pub(super) path: PathBuf,
    pub(super) frames: Vec<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Frame {
    pub(super) index: usize,
    pub(super) at_ms: u64,
    pub(super) reason: Reason,
    pub(super) marks: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) crop: Option<Rect>,
    pub(super) path: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MarkFrame {
    pub(super) number: usize,
    pub(super) from_ms: u64,
    pub(super) to_ms: u64,
    pub(super) frame: Option<usize>,
}

impl Detail {
    fn of(entry: &Entry, show: &ReadShow) -> Self {
        let manifest = &show.manifest;
        Self {
            ending: manifest.recording().ending,
            movie: show.movie.clone().map(|timing| Movie {
                path: entry.file(super::store::movie::FILE),
                timing,
            }),
            narration_lost: manifest.recording().narration_lost,
            summary: entry.file(Bundle::SUMMARY),
            manifest: entry.file(Manifest::FILE),
            sheets: manifest
                .sheets()
                .map(|(name, frames)| Sheet {
                    path: entry.file(&name),
                    frames: frames.iter().map(|frame| frame.index).collect(),
                })
                .collect(),
            frames: manifest
                .keyframes()
                .iter()
                .map(|frame| Frame {
                    index: frame.index,
                    at_ms: frame.at.as_millis(),
                    reason: frame.reason,
                    marks: manifest
                        .marks()
                        .get(frame.marks.clone())
                        .unwrap_or_default()
                        .iter()
                        .map(|mark| mark.number)
                        .collect(),
                    crop: frame.crop,
                    path: entry.file(&frame.file()),
                })
                .collect(),
            mark_frames: manifest
                .marks()
                .iter()
                .enumerate()
                .map(|(index, mark)| MarkFrame {
                    number: mark.number,
                    from_ms: mark.span.start().as_millis(),
                    to_ms: mark.span.end().as_millis(),
                    frame: manifest
                        .keyframes()
                        .iter()
                        .find(|frame| frame.marks.contains(&index))
                        .map(|frame| frame.index),
                })
                .collect(),
        }
    }
}

impl Inspection {
    pub(super) fn of(entry: &Entry, parent: Option<&Path>) -> Self {
        Self {
            overview: Overview::of(entry, parent),
            detail: entry.show.as_ref().ok().map(|show| Detail::of(entry, show)),
        }
    }
}

pub(super) fn list(
    entries: impl Iterator<Item = io::Result<Entry>>,
    parent: Option<&Path>,
) -> io::Result<Vec<Overview>> {
    let saved = entries.map(|entry| entry.map(|entry| Overview::of(&entry, parent)));
    let Some(shows) = parent else {
        return saved.collect();
    };
    let found = checkpoint::list(shows, SystemTime::now());
    let interrupted = found.shows.into_iter().map(|show| Overview {
        id: Some(show.id()),
        directory: show.directory().to_owned(),
        status: if show.narration_pending() {
            Status::NarrationPending
        } else {
            Status::Interrupted
        },
        expires_at: show.expires_at(),
        pinned: Some(false),
        duration_ms: None,
        keyframes: None,
        marks: None,
        problems: Vec::new(),
    });
    let unreadable = found.problems.into_iter().map(|problem| Overview {
        id: None,
        directory: problem.directory.clone(),
        status: Status::Damaged,
        expires_at: None,
        pinned: None,
        duration_ms: None,
        keyframes: None,
        marks: None,
        problems: vec![Problem {
            file: problem.directory,
            reason: problem.message,
        }],
    });
    interrupted.chain(unreadable).map(Ok).chain(saved).collect()
}
