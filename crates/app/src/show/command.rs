//! Headless Show commands. Inspection reports paths and metadata; private text stays in files.

mod output;
mod source;

use std::{ffi::OsString, io, path::PathBuf, time::SystemTime};

use anyhow::Context as _;
use speakeasy_show::{Demo, Typeface};

use super::store::{self, address, recall, retention};

const SKILL: &str = include_str!("../../../../skills/speakeasy-show/SKILL.md");
/// The demo's keyframes render as a Show records a 2880 × 1800 display, halved to fit
/// [`super::recorder::LONGEST_EDGE`].
const DEMO_SIZE: (u32, u32) = (1_440, 900);

const HELP: &str = "speakeasy show list [--json]
speakeasy show inspect SHOW [--json]
speakeasy show frames SHOW [FOCUS] [--around MS] [--json]
speakeasy show frames SHOW FOCUS --out NEW.png [--most N] [--around MS] [--crop] [--json]
speakeasy show frame SHOW FOCUS --out NEW.png [--crop] [--json]
speakeasy show keep SHOW [--off] [--json]
speakeasy show export SHOW --out NEW.zip [--json]
speakeasy show discard INTERRUPTED_SHOW [--json]
speakeasy show skill
speakeasy show demo [--out NEW_DIR]

list discovers interrupted and saved Shows, including damaged bundles.
inspect checks an explicit bundle and maps its sheets, keyframes and marks.
Both print metadata and current absolute paths, never narration or screen content.
Images are checked as files, not decoded. Missing files are reported, not repaired.
frames lists retained source times and sampling coverage, without reading pixels.
FOCUS is exactly one of --at M:SS.mmm, --mark N, or --phrase TEXT [--match K].
Phrase queries search recorded words, not summary edits, and require word timings.
With several phrase matches, frames lists locations; choose --match before exporting.
--around widens the focus span by milliseconds on each side (default 1000, maximum 10000).
frames --out exports a source contact sheet (--most defaults to 6, maximum 12).
Each cell reports its actual sample time; carried-in frames may precede the window.
--crop requires --mark and keeps surrounding context; source images have no ink.
frame exports the retained image at or before a time, or the first image before capture began.
It reports the actual sample time, excludes annotations, and never overwrites a file.
Source frames are available only in Shows saved by versions that retain them.
inspect exits with status 1 for a damaged or unsupported Show.
SHOW is an exact folder-name ID in Speakeasy's Shows folder, or an explicit path.
Use ./ID to address a relative path. No command chooses the latest Show.
keep prevents expiry; --off restores the original seven-day expiry date.
An overdue Show then expires at the next cleanup. Only managed Shows can be kept.
export writes a new ZIP with saved images, narration, retained samples, and a relative reading guide.
It preserves summary edits, excludes recovery audio and unrelated files, and never overwrites.
Exported copies do not expire automatically. Reports print paths and sizes, never private text.
discard permanently removes an explicit interrupted:ID or checkpoint path from list,
including pending audio. Saved Show bundles are unchanged.
Recover interrupted Shows in the Shows window; recovery leaves the clipboard untouched.
skill prints the agent reading guide for this installed version.
demo writes a simulated Show and prints its summary.
All commands work without a display, microphone, speech engine or running app.";

/// What the command line asks of saved Shows. Retention changes require an explicit target.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    List {
        json: bool,
    },
    Inspect {
        directory: PathBuf,
        json: bool,
    },
    Source(source::Command),
    Export {
        directory: PathBuf,
        destination: PathBuf,
        json: bool,
    },
    Keep {
        directory: PathBuf,
        kept: bool,
        json: bool,
    },
    Discard {
        directory: PathBuf,
        json: bool,
    },
    Skill,
    Demo {
        out: Option<PathBuf>,
    },
}

impl Command {
    pub(crate) fn parse(mut args: impl Iterator<Item = OsString>) -> anyhow::Result<Self> {
        let Some(verb) = args.next() else {
            return Ok(Self::Help);
        };
        if verb == "--help" || verb == "-h" {
            return Ok(Self::Help);
        }
        if verb == "frames" || verb == "frame" {
            return source::Command::parse(verb == "frame", args).map(Self::Source);
        }
        let mut json = false;
        let mut off = false;
        let mut path = None;
        let mut destination = None;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--help" | "-h") => return Ok(Self::Help),
                Some("--json")
                    if (verb == "list"
                        || verb == "inspect"
                        || verb == "keep"
                        || verb == "export"
                        || verb == "discard")
                        && !json =>
                {
                    json = true;
                },
                Some("--off") if verb == "keep" && !off => off = true,
                Some("--out") if verb == "export" && destination.is_none() => {
                    destination = Some(PathBuf::from(
                        args.next()
                            .filter(|path| {
                                !path.is_empty() && !path.to_string_lossy().starts_with('-')
                            })
                            .context("--out needs a new ZIP file path")?,
                    ));
                },
                Some("--out") if verb == "demo" && path.is_none() => {
                    path = Some(PathBuf::from(
                        args.next()
                            .filter(|path| !path.is_empty())
                            .context("--out needs a new directory")?,
                    ));
                },
                _ if (verb == "inspect"
                    || verb == "keep"
                    || verb == "discard"
                    || verb == "export")
                    && path.is_none()
                    && !arg.is_empty()
                    && !arg.to_string_lossy().starts_with('-') =>
                {
                    path = Some(PathBuf::from(arg));
                },
                _ => anyhow::bail!(
                    "Unknown or repeated show argument; speakeasy show --help lists the commands"
                ),
            }
        }
        match verb.to_str() {
            Some("list") => Ok(Self::List { json }),
            Some("export") => Ok(Self::Export {
                directory: path.context("show export needs an exact Show ID or path")?,
                destination: destination.context("show export needs --out NEW.zip")?,
                json,
            }),
            Some("inspect") => Ok(Self::Inspect {
                directory: path.context("show inspect needs an exact Show ID or path")?,
                json,
            }),
            Some("keep") => Ok(Self::Keep {
                directory: path.context("show keep needs an exact Show ID or path")?,
                kept: !off,
                json,
            }),
            Some("discard") => Ok(Self::Discard {
                directory: path
                    .context("show discard needs an explicit interrupted Show ID or path")?,
                json,
            }),
            Some("skill") => Ok(Self::Skill),
            Some("demo") => Ok(Self::Demo { out: path }),
            _ => anyhow::bail!("Unknown show command; speakeasy show --help lists the commands"),
        }
    }

    pub(crate) fn run(self) -> anyhow::Result<()> {
        match self.write(&mut io::stdout().lock()) {
            Err(error)
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe) =>
            {
                Ok(())
            },
            result => result,
        }
    }

    fn write(self, out: &mut impl io::Write) -> anyhow::Result<()> {
        match self {
            Self::Help => writeln!(out, "{HELP}")?,
            Self::Skill => write!(out, "{SKILL}")?,
            Self::Source(command) => command.write(out)?,
            Self::Export {
                directory,
                destination,
                json,
            } => {
                let directory =
                    address::resolve(&directory, || store::directory().map_err(io::Error::other))?;
                let exported = store::export::write(&directory, &destination)?;
                if json {
                    output::json(&exported, out)?;
                } else {
                    writeln!(
                        out,
                        "Exported {} files ({} bytes) to {}",
                        exported.files,
                        exported.bytes,
                        exported.path.display()
                    )?;
                }
            },
            Self::List { json } => {
                let directory = store::directory()?;
                let entries = recall::list(&directory)
                    .with_context(|| format!("Cannot list Shows in {}", directory.display()))?;
                output::list(entries, Some(&directory), json, out)?;
            },
            Self::Inspect { directory, json } => {
                let directory =
                    address::resolve(&directory, || store::directory().map_err(io::Error::other))?;
                let entry = recall::Entry::open(&directory)
                    .with_context(|| format!("Cannot open Show {}", directory.display()))?;
                let healthy = entry
                    .show
                    .as_ref()
                    .is_ok_and(|show| show.problems.is_empty());
                output::inspect(entry, store::directory().ok().as_deref(), json, out)?;
                anyhow::ensure!(
                    healthy,
                    "The Show is damaged or unsupported; inspection lists the problems."
                );
            },
            Self::Keep {
                directory,
                kept,
                json,
            } => {
                let parent = store::directory()?;
                let directory = address::resolve(&directory, || Ok(parent.clone()))?;
                let retention =
                    retention::set_kept(&parent, &directory, kept).with_context(|| {
                        format!("Cannot change retention for {}", directory.display())
                    })?;
                output::kept(&directory, retention, json, out)?;
            },
            Self::Discard { directory, json } => {
                let interrupted = store::checkpoint::resolve(&store::directory()?, &directory)?;
                interrupted.lock()?.discard()?;
                if json {
                    output::json(
                        &serde_json::json!({"id": interrupted.id(), "discarded": true}),
                        out,
                    )?;
                } else {
                    writeln!(out, "Discarded {}", interrupted.id())?;
                }
            },
            Self::Demo { out: directory } => demo(directory, out)?,
        }
        Ok(())
    }
}

fn demo(directory: Option<PathBuf>, out: &mut impl io::Write) -> anyhow::Result<()> {
    let directory = directory.map(std::path::absolute).transpose()?;
    if let Some(directory) = &directory {
        anyhow::ensure!(
            !directory.exists(),
            "{} already exists; choose a new directory",
            directory.display()
        );
    }
    let bundle = Demo::new(Typeface::parse(crate::BRAND_FONT_FILE)?).bundle(DEMO_SIZE)?;
    let directory = match directory {
        Some(directory) => directory,
        None => store::fresh_directory(&store::private_directory()?, SystemTime::now())?,
    };
    let (_, summary) = store::stage(&directory, bundle, None)?.persist()?;
    write!(out, "{summary}")?;
    Ok(())
}

#[cfg(test)]
mod tests;
