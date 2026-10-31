//! The CLI's metadata-only projections. Never serialize the manifest or a keyframe's free text.

use std::{
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::Serialize;
use speakeasy_show::{Bundle, Ending, Manifest, Moment, NarrationLoss, Reason, Unreadable};

use super::recall::{Entry, Health, ReadError, ReadShow};
use crate::show::store::{address, retention::Retention};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum Status {
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
    const fn text(self) -> &'static str {
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
struct Overview {
    id: Option<String>,
    directory: PathBuf,
    status: Status,
    pinned: Option<bool>,
    expires_at: Option<String>,
    duration_ms: Option<u64>,
    keyframes: Option<usize>,
    marks: Option<usize>,
    problems: Vec<Problem>,
}

#[derive(Serialize)]
struct Problem {
    file: PathBuf,
    reason: String,
}

impl Overview {
    fn of(entry: &Entry, parent: Option<&Path>) -> Self {
        let (status, problems) = match &entry.show {
            Ok(show) => (
                entry.health().into(),
                show.problems
                    .iter()
                    .map(|problem| Problem {
                        file: entry.file(&problem.name),
                        reason: problem.reason.clone(),
                    })
                    .collect(),
            ),
            Err(error) => {
                let (status, name, reason) = match error {
                    ReadError::Manifest(Unreadable::Unsupported(_)) => {
                        (Status::Unsupported, Manifest::FILE, error.to_string())
                    },
                    ReadError::Manifest(_) => (Status::Damaged, Manifest::FILE, error.to_string()),
                    ReadError::File(problem) => (
                        Status::Damaged,
                        problem.name.as_str(),
                        problem.reason.clone(),
                    ),
                };
                (
                    status,
                    vec![Problem {
                        file: entry.file(name),
                        reason,
                    }],
                )
            },
        };
        let show = entry.show.as_ref().ok();
        // Identity is omitted unless membership can be established. An unavailable storage root
        // must not prevent inspecting a readable moved bundle or advertise a misleading ID.
        let id =
            parent.and_then(|parent| address::managed_id(parent, &entry.directory).ok().flatten());
        let retention = id
            .as_ref()
            .and_then(|_| Retention::read(&entry.directory).ok());
        Self {
            id: id.map(|id| id.as_str().to_owned()),
            directory: entry.directory.clone(),
            status,
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
struct Inspection {
    #[serde(flatten)]
    overview: Overview,
    #[serde(flatten)]
    detail: Option<Detail>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Detail {
    ending: Ending,
    narration_lost: Option<NarrationLoss>,
    summary: PathBuf,
    manifest: PathBuf,
    sheets: Vec<Sheet>,
    frames: Vec<Frame>,
    mark_frames: Vec<MarkFrame>,
    movie: Option<Movie>,
}

#[derive(Serialize)]
struct Movie {
    path: PathBuf,
    #[serde(flatten)]
    timing: super::super::store::movie::Metadata,
}

#[derive(Serialize)]
struct Sheet {
    path: PathBuf,
    frames: Vec<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Frame {
    index: usize,
    at_ms: u64,
    reason: Reason,
    marks: Vec<usize>,
    path: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MarkFrame {
    number: usize,
    from_ms: u64,
    to_ms: u64,
    frame: Option<usize>,
}

impl Detail {
    fn of(entry: &Entry, show: &ReadShow) -> Self {
        let manifest = &show.manifest;
        Self {
            ending: manifest.recording().ending,
            movie: show.movie.clone().map(|timing| Movie {
                path: entry.file(super::super::store::movie::FILE),
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

pub(super) fn json(value: &impl Serialize, out: &mut impl io::Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value).map_err(|error| {
        io::Error::new(
            error.io_error_kind().unwrap_or(io::ErrorKind::InvalidData),
            "Cannot write Show metadata",
        )
    })?;
    writeln!(out)
}

pub(super) fn list(
    entries: impl Iterator<Item = io::Result<Entry>>,
    parent: Option<&Path>,
    as_json: bool,
    out: &mut impl io::Write,
) -> io::Result<()> {
    let mut entries: Vec<_> = entries
        .map(|entry| entry.map(|entry| Overview::of(&entry, parent)))
        .collect::<io::Result<_>>()?;
    if let Some(parent) = parent {
        let found = crate::show::store::checkpoint::list(parent, SystemTime::now());
        let mut interrupted: Vec<_> = found
            .shows
            .into_iter()
            .map(|show| Overview {
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
            })
            .collect();
        interrupted.extend(found.problems.into_iter().map(|problem| Overview {
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
        }));
        interrupted.append(&mut entries);
        entries = interrupted;
    }
    if as_json {
        return json(&entries, out);
    }
    if entries.is_empty() {
        return writeln!(out, "No saved Shows.");
    }
    for entry in entries {
        write!(out, "{}", entry.status.text())?;
        if let (Some(duration), Some(frames)) = (entry.duration_ms, entry.keyframes) {
            write!(
                out,
                " · {} · {frames} frames",
                Moment::from_millis(duration)
            )?;
        }
        if entry.pinned == Some(true) {
            write!(out, " · kept")?;
        }
        writeln!(out, " · {}", entry.directory.display())?;
        problems(&entry.problems, out)?;
    }
    Ok(())
}

pub(super) fn inspect(
    entry: Entry,
    parent: Option<&Path>,
    as_json: bool,
    out: &mut impl io::Write,
) -> io::Result<()> {
    let report = Inspection {
        overview: Overview::of(&entry, parent),
        detail: entry
            .show
            .as_ref()
            .ok()
            .map(|show| Detail::of(&entry, show)),
    };
    if as_json {
        return json(&report, out);
    }
    writeln!(
        out,
        "Show: {}\nStatus: {}",
        report.overview.directory.display(),
        report.overview.status.text()
    )?;
    if let Some(id) = &report.overview.id {
        writeln!(out, "ID: {id}")?;
    }
    if let Some(expiry) = &report.overview.expires_at {
        writeln!(out, "Expires: {expiry}")?;
    }
    if let Some(detail) = report.detail {
        let duration = report.overview.duration_ms.unwrap_or_default();
        writeln!(
            out,
            "Duration: {} ({duration} ms)",
            Moment::from_millis(duration)
        )?;
        writeln!(out, "Kept: {}", report.overview.pinned.unwrap_or_default())?;
        writeln!(
            out,
            "Ending: {}",
            match detail.ending {
                Ending::Finished => "finished",
                Ending::ScreenStopped => "screen stopped",
                Ending::WindowClosed => "window closed",
                Ending::WindowOffScreen => "window off screen",
            }
        )?;
        if let Some(lost) = detail.narration_lost {
            writeln!(
                out,
                "Narration missing: {}",
                match lost {
                    NarrationLoss::Incomplete => "only checkpointed audio recovered",
                    NarrationLoss::AudioStopped => "audio stopped",
                    NarrationLoss::NotTranscribed => "transcription failed",
                }
            )?;
        }
        writeln!(
            out,
            "Summary: {}\nManifest: {}",
            detail.summary.display(),
            detail.manifest.display()
        )?;
        if let Some(movie) = detail.movie {
            writeln!(
                out,
                "{}",
                movie
                    .timing
                    .note(&movie.path)
                    .map_err(io::Error::other)?
                    .trim()
            )?;
        }
        for sheet in detail.sheets {
            writeln!(
                out,
                "Sheet, frames {:?}: {}",
                sheet.frames,
                sheet.path.display()
            )?;
        }
        for frame in detail.frames {
            writeln!(
                out,
                "#{} at {} ({} ms), marks {:?}: {}",
                frame.index,
                Moment::from_millis(frame.at_ms),
                frame.at_ms,
                frame.marks,
                frame.path.display()
            )?;
        }
        for mark in detail.mark_frames {
            match mark.frame {
                Some(frame) => writeln!(out, "Mark {}: frame #{frame}", mark.number)?,
                None => writeln!(out, "Mark {}: no saved frame", mark.number)?,
            }
        }
    }
    problems(&report.overview.problems, out)
}

pub(super) fn kept(
    directory: &Path,
    retention: Retention,
    as_json: bool,
    out: &mut impl io::Write,
) -> io::Result<()> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Change<'a> {
        directory: &'a Path,
        pinned: bool,
        expires_at: Option<String>,
    }
    if as_json {
        return json(
            &Change {
                directory,
                pinned: retention == Retention::Kept,
                expires_at: retention.expires_at(),
            },
            out,
        );
    }
    writeln!(
        out,
        "{}: {}.",
        directory.display(),
        retention.label(SystemTime::now())
    )
}

fn problems(problems: &[Problem], out: &mut impl io::Write) -> io::Result<()> {
    for problem in problems {
        writeln!(
            out,
            "Problem: {}: {}",
            problem.file.display(),
            problem.reason
        )?;
    }
    Ok(())
}
