//! Shows: screen recordings with narration and ink, written as bundles a coding agent reads.

mod overlay;
mod publish;
mod recorder;
mod retention;

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use speakeasy_show::{Demo, Moment, Sample, Signature, Typeface, budget, compose, select};

pub(crate) use self::{
    overlay::{Canvas, attend as attend_overlay},
    publish::publish,
    recorder::Recorder,
    retention::attend as attend_retention,
};

/// The demo renders frames this small just to fingerprint them; a fingerprint's 128 × 72 grid
/// needs no more, and rendering costs grow with every pixel.
const SAMPLE_SIZE: (u32, u32) = (320, 200);
/// The demo's keyframes render as a Show records a 2880 × 1800 display, halved to fit
/// [`recorder::LONGEST_EDGE`].
const DEMO_SIZE: (u32, u32) = (1_440, 900);

/// A Show subcommand from the command line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Builds the scripted demo Show, into `out`, which must not exist yet, or a new directory
    /// beside other Shows.
    Demo { out: Option<PathBuf> },
}

impl Command {
    /// Parses the arguments after `show`.
    pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
        let subcommand = args.next().context("show needs a subcommand: demo")?;
        anyhow::ensure!(
            subcommand == "demo",
            "Unknown show subcommand: {subcommand}"
        );
        let mut out = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--out" => out = Some(args.next().context("--out needs a directory")?.into()),
                _ => anyhow::bail!("Unknown show option: {arg}"),
            }
        }
        Ok(Self::Demo { out })
    }

    /// Runs the subcommand, printing the bundle's summary.
    pub(crate) fn run(self) -> anyhow::Result<()> {
        let Self::Demo { out } = self;
        let out = out.map(std::path::absolute).transpose()?;
        if let Some(out) = &out {
            anyhow::ensure!(
                !out.exists(),
                "{} already exists; choose a new directory",
                out.display()
            );
        }
        let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
        let demo = Demo::new(typeface.clone());
        let recording = demo.recording();
        let duration = recording.duration.since(Moment::START);
        let samples = std::iter::successors(Some(Duration::ZERO), |at| {
            Some(at.saturating_add(recorder::SAMPLE_INTERVAL)).filter(|next| *next <= duration)
        })
        .map(|at| {
            let at = Moment::from(at);
            let frame = demo.frame(at, SAMPLE_SIZE.0, SAMPLE_SIZE.1)?;
            Ok(Sample {
                at,
                signature: Signature::of(&frame),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
        let selection = select(&recording, &samples, budget(duration));
        let frames = selection
            .keyframes()
            .iter()
            .map(|keyframe| demo.frame(keyframe.at, DEMO_SIZE.0, DEMO_SIZE.1))
            .collect::<Result<Vec<_>, _>>()?;
        let bundle = compose(recording, selection, frames, &typeface)?;
        let directory = match out {
            Some(out) => out,
            None => fresh_directory(&directory()?, SystemTime::now())?,
        };
        write(&directory, &bundle.files(&directory)?)?;
        print!("{}", bundle.summary(&directory));
        Ok(())
    }
}

/// Where Shows live: machine-local, beside the engines and models.
fn directory() -> anyhow::Result<PathBuf> {
    Ok(crate::paths::data_root()?.join("shows"))
}

/// An unused directory under `parent` named for `now` in UTC, such as `20261019T143603Z`.
fn fresh_directory(parent: &Path, now: SystemTime) -> anyhow::Result<PathBuf> {
    let stamp = utc_stamp(now);
    (1..=100_u8)
        .map(|attempt| match attempt {
            1 => parent.join(&stamp),
            _ => parent.join(format!("{stamp}-{attempt}")),
        })
        .find(|candidate| !candidate.exists())
        .context("Too many Shows were written this second")
}

/// Writes `files` into `directory`, which must not exist, all at once.
fn write(directory: &Path, files: &[speakeasy_show::File]) -> anyhow::Result<()> {
    stage(directory, files)?.persist()
}

/// A bundle written to a hidden sibling of its directory. It appears there only once persisted,
/// so a reader never sees half a bundle, and dropping it unpersisted deletes it.
struct Staged {
    staging: tempfile::TempDir,
    directory: PathBuf,
}

impl Staged {
    /// Moves the bundle into place.
    fn persist(mut self) -> anyhow::Result<()> {
        fs::rename(self.staging.path(), &self.directory)
            .with_context(|| format!("Cannot create {}", self.directory.display()))?;
        // The staging directory is now the bundle.
        self.staging.disable_cleanup(true);
        Ok(())
    }
}

/// Writes `files` beside `directory`, which must not exist yet. Shows are private to the user.
fn stage(directory: &Path, files: &[speakeasy_show::File]) -> anyhow::Result<Staged> {
    anyhow::ensure!(
        !directory.exists(),
        "{} already exists, and a Show never replaces one",
        directory.display()
    );
    let parent = directory
        .parent()
        .context("A Show's directory needs a parent")?;
    fs::create_dir_all(parent).with_context(|| format!("Cannot create {}", parent.display()))?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".partial-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    // Windows already limits the user's local application data to the user.
    let staging = builder
        .tempdir_in(parent)
        .with_context(|| format!("Cannot write in {}", parent.display()))?;
    for file in files {
        let path = staging.path().join(&file.name);
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        fs::write(&path, &file.contents)
            .with_context(|| format!("Cannot write {}", path.display()))?;
    }
    Ok(Staged {
        staging,
        directory: directory.to_path_buf(),
    })
}

/// `now` in UTC as `YYYYMMDDTHHMMSSZ`, which sorts in time order.
fn utc_stamp(now: SystemTime) -> String {
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (days, time) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_date(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        time / 3_600,
        time / 60 % 60,
        time % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01, after Howard Hinnant's `civil_from_days`.
fn civil_date(days: u64) -> (u64, u64, u64) {
    // Count years from March, so each year's leap day falls at its end.
    let shifted = days.saturating_add(719_468);
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era = day_of_era
        .saturating_sub(day_of_era / 1_460)
        .saturating_add(day_of_era / 36_524)
        .saturating_sub(day_of_era / 146_096)
        / 365;
    let day_of_year = day_of_era.saturating_sub(
        year_of_era
            .saturating_mul(365)
            .saturating_add(year_of_era / 4)
            .saturating_sub(year_of_era / 100),
    );
    let month_index = day_of_year.saturating_mul(5).saturating_add(2) / 153;
    let day = day_of_year
        .saturating_sub(month_index.saturating_mul(153).saturating_add(2) / 5)
        .saturating_add(1);
    let month = if month_index < 10 {
        month_index.saturating_add(3)
    } else {
        month_index.saturating_sub(9)
    };
    let year = era
        .saturating_mul(400)
        .saturating_add(year_of_era)
        .saturating_add(u64::from(month <= 2));
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))
            .unwrap()
    }

    #[test]
    fn stamps_name_utc_moments_in_order() {
        assert_eq!(utc_stamp(at(0)), "19700101T000000Z");
        assert_eq!(utc_stamp(at(951_782_400)), "20000229T000000Z");
        assert_eq!(utc_stamp(at(1_792_420_563)), "20261019T143603Z");
        assert_eq!(utc_stamp(at(4_102_444_799)), "20991231T235959Z");
    }

    #[test]
    fn a_bundle_appears_whole_under_a_fresh_name() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let parent = root.path().join("shows");
        let first = fresh_directory(&parent, at(1_792_420_563))?;
        let files = [
            speakeasy_show::File {
                name: "summary.md".into(),
                contents: b"text".to_vec(),
            },
            speakeasy_show::File {
                name: "frames/frame-01.png".into(),
                contents: vec![1, 2],
            },
        ];
        write(&first, &files)?;
        assert_eq!(fs::read(first.join("frames/frame-01.png"))?, [1, 2]);
        assert!(
            write(&first, &files).is_err(),
            "an existing bundle is never replaced"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(&first)?.permissions().mode() & 0o777, 0o700);
        }
        let second = fresh_directory(&parent, at(1_792_420_563))?;
        assert_eq!(second.file_name(), Some("20261019T143603Z-2".as_ref()));
        let names: Vec<_> = fs::read_dir(&parent)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<_, _>>()?;
        assert_eq!(names, ["20261019T143603Z"]);
        Ok(())
    }

    #[test]
    fn show_takes_one_subcommand_and_an_output() -> anyhow::Result<()> {
        let parse = |args: &[&str]| Command::parse(args.iter().map(|arg| (*arg).to_owned()));
        assert_eq!(parse(&["demo"])?, Command::Demo { out: None });
        assert_eq!(
            parse(&["demo", "--out", "here"])?,
            Command::Demo {
                out: Some("here".into())
            }
        );
        assert!(parse(&[]).is_err());
        assert!(parse(&["record"]).is_err());
        assert!(parse(&["demo", "--out"]).is_err());
        Ok(())
    }
}
