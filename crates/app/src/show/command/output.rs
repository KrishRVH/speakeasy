use std::{io, path::Path, time::SystemTime};

use serde::Serialize;
use speakeasy_show::{Ending, Moment, NarrationLoss};

use super::recall::Entry;
use crate::show::{
    evidence::{self, Inspection, Problem},
    store::retention::Retention,
};

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
    let entries = evidence::list(entries, parent)?;
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
    let report = Inspection::of(&entry, parent);
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

#[cfg(test)]
mod tests;
