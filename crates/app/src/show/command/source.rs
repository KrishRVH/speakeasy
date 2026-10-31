//! Explicit source queries and PNG exports. Reports contain provenance, never private text.

mod query;
mod video;

use std::{ffi::OsString, io, path::PathBuf, time::Duration};

use anyhow::Context as _;
use query::Query;
use serde::Serialize;
use speakeasy_show::{
    Manifest, Moment, Rect, Typeface,
    locate::{self, Evidence},
};

use crate::show::store::{self, address, recall, source};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Read(Request),
}

#[derive(PartialEq, Eq)]
pub(crate) struct Request {
    directory: PathBuf,
    focus: Option<Query>,
    around: u64,
    most: usize,
    output: Output,
    json: bool,
    video: bool,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("SourceRequest").finish_non_exhaustive()
    }
}

#[derive(PartialEq, Eq)]
enum Output {
    List,
    Frame { destination: PathBuf, crop: bool },
    Sheet { destination: PathBuf, crop: bool },
}

impl Command {
    pub(super) fn parse(
        single: bool,
        mut args: impl Iterator<Item = OsString>,
    ) -> anyhow::Result<Self> {
        let (mut directory, mut focus, mut destination, mut json) = (None, None, None, false);
        let (mut around, mut most, mut occurrence, mut crop) = (None, None, None, false);
        let mut video = false;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--help" | "-h") => return Ok(Self::Help),
                Some("--json") if !json => json = true,
                Some("--video") if !video => video = true,
                Some("--at" | "--mark" | "--phrase") if focus.is_none() => {
                    let value = value(&mut args)?;
                    focus = Some(match arg.to_str() {
                        Some("--at") => Query::At(query::time(&value)?),
                        Some("--mark") => Query::Mark(positive(&value)?),
                        _ => Query::Phrase(value),
                    });
                },
                Some("--match") if occurrence.is_none() => {
                    occurrence = Some(positive(&value(&mut args)?)?);
                },
                Some("--around") if !single && around.is_none() => {
                    let parsed = value(&mut args)?
                        .parse::<u64>()
                        .context("--around needs milliseconds from 0 to 10000")?;
                    anyhow::ensure!(
                        parsed <= 10_000,
                        "--around accepts at most 10000 milliseconds"
                    );
                    around = Some(parsed);
                },
                Some("--most") if !single && most.is_none() => {
                    let parsed = positive(&value(&mut args)?)?;
                    anyhow::ensure!(parsed <= 12, "--most accepts at most 12 frames");
                    most = Some(parsed);
                },
                Some("--crop") if !crop => crop = true,
                Some("--out") if destination.is_none() => {
                    destination = Some(PathBuf::from(
                        args.next()
                            .filter(|arg| {
                                !arg.is_empty() && !arg.to_string_lossy().starts_with('-')
                            })
                            .context("--out needs a new PNG path")?,
                    ));
                },
                _ if directory.is_none()
                    && !arg.is_empty()
                    && !arg.to_string_lossy().starts_with('-') =>
                {
                    directory = Some(PathBuf::from(arg));
                },
                _ => anyhow::bail!(
                    "Unknown, repeated, or conflicting source argument; see speakeasy show --help"
                ),
            }
        }
        anyhow::ensure!(!video || single, "--video supports show frame");
        anyhow::ensure!(
            occurrence.is_none() || matches!(focus, Some(Query::Phrase(_))),
            "--match requires --phrase"
        );
        if let (Some(Query::Phrase(phrase)), Some(occurrence)) = (&focus, occurrence) {
            focus = Some(Query::Match {
                phrase: phrase.clone(),
                occurrence,
            });
        }
        anyhow::ensure!(
            !crop || matches!(focus, Some(Query::Mark(_))),
            "--crop requires --mark"
        );
        anyhow::ensure!(
            focus.is_some()
                || (!single && destination.is_none() && around.is_none() && most.is_none()),
            "Choose exactly one focus: --at, --mark, or --phrase"
        );
        anyhow::ensure!(
            destination.is_some() || (most.is_none() && !crop),
            "--most and --crop require --out NEW.png"
        );
        let output = match (single, destination) {
            (true, Some(destination)) => Output::Frame { destination, crop },
            (false, Some(destination)) => Output::Sheet { destination, crop },
            (false, None) => Output::List,
            (true, None) => anyhow::bail!("show frame needs a focus and --out NEW.png"),
        };
        Ok(Self::Read(Request {
            directory: directory.context("Source inspection needs an exact Show ID or path")?,
            focus,
            around: around.unwrap_or(1_000),
            most: most.unwrap_or(6),
            output,
            json,
            video,
        }))
    }

    pub(super) fn write(self, out: &mut impl io::Write) -> anyhow::Result<()> {
        let Self::Read(request) = self else {
            return super::Command::Help.write(out);
        };
        request.write(out)
    }
}

fn value(args: &mut impl Iterator<Item = OsString>) -> anyhow::Result<String> {
    args.next()
        .and_then(|arg| arg.into_string().ok())
        .filter(|arg| !arg.is_empty())
        .context("The source option needs a nonempty UTF-8 value")
}

fn positive(value: &str) -> anyhow::Result<usize> {
    value
        .parse()
        .ok()
        .filter(|number| *number > 0)
        .context("Use a positive whole number")
}

impl Request {
    fn write(self, out: &mut impl io::Write) -> anyhow::Result<()> {
        let directory = address::resolve(&self.directory, || {
            store::directory().map_err(io::Error::other)
        })?;
        if self.video {
            return video::write(self, &directory, out);
        }
        anyhow::ensure!(
            source::size(&directory)?.is_some(),
            "This Show has no retained source frames. Its selected keyframes may still be available through show inspect."
        );
        let index = source::inspect(&directory)?;
        let Some(query) = &self.focus else {
            return coverage(&directory, &index, self.json, out);
        };
        let manifest = recall::Entry::open(&directory)?.show?.manifest;
        anyhow::ensure!(
            index.duration() == manifest.recording().duration,
            "Source and Show timelines differ; inspect the bundle before exporting"
        );
        let found = query.resolve(&manifest)?;
        let mut report = Report::new(&manifest, &found);
        let Some(span) = found.selected else {
            anyhow::ensure!(
                matches!(self.output, Output::List),
                "The phrase has several matches; list them with show frames, then choose --match N"
            );
            return report.write(self.json, out);
        };
        let window = locate::window(span, Duration::from_millis(self.around), index.duration());
        let crop_window = match self.output {
            Output::Frame { crop: true, .. } => Some(span),
            Output::Sheet { crop: true, .. } => Some(window),
            _ => None,
        };
        anyhow::ensure!(
            crop_window.is_none_or(|window| index.stable_geometry(window)),
            "The captured surface resized in this window; export uncropped frames instead"
        );
        let most = match self.output {
            Output::Sheet { .. } => self.most,
            Output::List | Output::Frame { .. } => index.moments().len(),
        };
        let samples = locate::pick(index.moments(), window, span.start(), most);
        report.window_ms = Some([window.start().as_millis(), window.end().as_millis()]);
        report.carried_in_ms = samples.carried_in.map(Moment::as_millis);
        report.longest_gap_ms =
            Some(u64::try_from(samples.longest_gap.as_millis()).unwrap_or(u64::MAX));
        report.times_ms = samples.times.iter().map(|at| at.as_millis()).collect();
        report.keyframes = manifest
            .keyframes()
            .iter()
            .filter(|frame| window.contains(frame.at))
            .map(|frame| frame.index)
            .collect();
        report.marks = manifest
            .marks()
            .iter()
            .filter(|mark| mark.span.start() <= window.end() && mark.span.end() >= window.start())
            .map(|mark| mark.number)
            .collect();
        match self.output {
            Output::List => {},
            Output::Frame { destination, crop } => {
                let served = source::frame(&directory, &index, span.start())?;
                let (image, crop_px) =
                    locate::crop_image(served.image, crop_bounds(&manifest, query, crop)?)?;
                report.requested_ms = Some(span.start().as_millis());
                report.sampled_ms = Some(served.at.as_millis());
                report.cells.push(locate::Cell {
                    sampled_ms: served.at.as_millis(),
                    crop_px,
                });
                report.path = Some(export(destination, &image)?);
            },
            Output::Sheet { destination, crop } => {
                let evidence = Evidence::compose(
                    &samples.times,
                    crop_bounds(&manifest, query, crop)?,
                    |at| source::frame(&directory, &index, at),
                    &Typeface::parse(crate::BRAND_FONT_FILE)?,
                )?;
                report.cells = evidence.cells;
                report.path = Some(export(destination, &evidence.image)?);
            },
        }
        report.write(self.json, out)
    }
}

fn crop_bounds(manifest: &Manifest, query: &Query, crop: bool) -> anyhow::Result<Option<Rect>> {
    if !crop {
        return Ok(None);
    }
    let Query::Mark(number) = query else {
        anyhow::bail!("--crop requires --mark");
    };
    manifest
        .marks()
        .iter()
        .find(|mark| mark.number == *number)
        .map(|mark| Some(mark.bounds))
        .context("The selected mark is unavailable")
}

fn export(destination: PathBuf, image: &speakeasy_show::Image) -> anyhow::Result<PathBuf> {
    let destination = std::path::absolute(destination)?;
    source::export(&destination, &image.encode_png()?)
        .context("Cannot create the export; choose a new writable PNG path")?;
    Ok(destination)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report<'a> {
    focus: &'a query::Found,
    window_ms: Option<[u64; 2]>,
    carried_in_ms: Option<u64>,
    times_ms: Vec<u64>,
    longest_gap_ms: Option<u64>,
    keyframes: Vec<usize>,
    marks: Vec<usize>,
    narration: &'static str,
    cells: Vec<locate::Cell>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sampled_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    video: Option<video::Coverage>,
}

impl<'a> Report<'a> {
    fn new(manifest: &Manifest, focus: &'a query::Found) -> Self {
        let narration = match &manifest.recording().narration {
            speakeasy_show::Narration::Words(words) if words.is_empty() => "absent",
            speakeasy_show::Narration::Words(_) => {
                if manifest.recording().narration_lost.is_some() {
                    "incomplete"
                } else {
                    "timed"
                }
            },
            speakeasy_show::Narration::Text(_) => "untimed",
        };
        Self {
            focus,
            window_ms: None,
            carried_in_ms: None,
            times_ms: Vec::new(),
            longest_gap_ms: None,
            keyframes: Vec::new(),
            marks: Vec::new(),
            narration,
            cells: Vec::new(),
            path: None,
            requested_ms: None,
            sampled_ms: None,
            video: None,
        }
    }

    fn write(&self, json: bool, out: &mut impl io::Write) -> anyhow::Result<()> {
        if json {
            super::output::json(self, out)?;
            return Ok(());
        }
        for (index, span) in self.focus.matches.iter().enumerate() {
            writeln!(
                out,
                "Match {} · {}–{} ms",
                index.saturating_add(1),
                span.start().as_millis(),
                span.end().as_millis()
            )?;
        }
        if self.focus.selected.is_none() {
            writeln!(out, "Choose --match N to inspect one location.")?;
        }
        if let Some([start, end]) = self.window_ms {
            writeln!(
                out,
                "Window {start}–{end} ms · longest unsampled gap {} ms",
                self.longest_gap_ms.unwrap_or(0)
            )?;
            for at in &self.times_ms {
                writeln!(out, "Sampled {at} ms")?;
            }
        }
        if let Some(video) = &self.video {
            writeln!(
                out,
                "Video {}–{} ms · annotations through {} ms",
                video.span.start().as_millis(),
                video.span.end().as_millis(),
                video.annotations.as_millis()
            )?;
        }
        if let Some(path) = &self.path {
            writeln!(out, "{}", path.display())?;
            for cell in &self.cells {
                write!(out, "Sampled {} ms", cell.sampled_ms)?;
                if let Some(crop) = cell.crop_px {
                    write!(
                        out,
                        " · crop {} × {} px at {}, {}",
                        crop.width, crop.height, crop.left, crop.top
                    )?;
                }
                writeln!(out)?;
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Coverage {
    path: PathBuf,
    duration_ms: u64,
    arrivals: u64,
    retained: usize,
    stride: u64,
    longest_gap_ms: u64,
    bytes: u64,
    times_ms: Vec<u64>,
}

fn coverage(
    directory: &std::path::Path,
    index: &speakeasy_show::Source,
    json: bool,
    out: &mut impl io::Write,
) -> anyhow::Result<()> {
    if json {
        super::output::json(
            &Coverage {
                path: directory.join(source::FILE),
                duration_ms: index.duration().as_millis(),
                arrivals: index.arrivals(),
                retained: index.moments().len(),
                stride: index.stride(),
                longest_gap_ms: source::longest_gap(index),
                bytes: index.bytes(),
                times_ms: index.moments().iter().map(|at| at.as_millis()).collect(),
            },
            out,
        )?;
    } else {
        writeln!(
            out,
            "{} retained frames from {} arrivals · stride {} · longest gap {} ms · {} bytes",
            index.moments().len(),
            index.arrivals(),
            index.stride(),
            source::longest_gap(index),
            index.bytes()
        )?;
        for at in index.moments() {
            writeln!(out, "{at} ({} ms)", at.as_millis())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
