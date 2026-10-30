//! Headless Show commands. Inspection reports paths and metadata; private text stays in files.

mod output;

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
speakeasy show keep SHOW [--off] [--json]
speakeasy show skill
speakeasy show demo [--out NEW_DIR]

list discovers saved Shows, newest first, including damaged bundles.
inspect checks an explicit bundle and maps its sheets, keyframes and marks.
Both print metadata and current absolute paths, never narration or screen content.
Images are checked as files, not decoded. Missing files are reported, not repaired.
inspect exits with status 1 for a damaged or unsupported Show.
SHOW is an exact folder-name ID in Speakeasy's Shows folder, or an explicit path.
Use ./ID to address a relative path. No command chooses the latest Show.
keep prevents expiry; --off restores the original seven-day expiry date.
An overdue Show then expires at the next cleanup. Only managed Shows can be kept.
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
    Keep {
        directory: PathBuf,
        kept: bool,
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
        let mut json = false;
        let mut off = false;
        let mut path = None;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--help" | "-h") => return Ok(Self::Help),
                Some("--json")
                    if (verb == "list" || verb == "inspect" || verb == "keep") && !json =>
                {
                    json = true;
                },
                Some("--off") if verb == "keep" && !off => off = true,
                Some("--out") if verb == "demo" && path.is_none() => {
                    path = Some(PathBuf::from(
                        args.next()
                            .filter(|path| !path.is_empty())
                            .context("--out needs a new directory")?,
                    ));
                },
                _ if (verb == "inspect" || verb == "keep")
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
            Some("inspect") => Ok(Self::Inspect {
                directory: path.context("show inspect needs an exact Show ID or path")?,
                json,
            }),
            Some("keep") => Ok(Self::Keep {
                directory: path.context("show keep needs an exact Show ID or path")?,
                kept: !off,
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
    let (_, summary) = store::stage(&directory, bundle)?.persist()?;
    write!(out, "{summary}")?;
    Ok(())
}

#[cfg(test)]
mod tests;
