mod output;
mod source;
mod video;

use std::{
    ffi::{OsStr, OsString},
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use speakeasy_show::{Demo, Typeface};

use super::{
    evidence::query::{self, Query},
    store::{self, address, recall, retention},
};

const SKILL: &str = include_str!("../../../../skills/speakeasy-show/SKILL.md");

const DEMO_SIZE: (u32, u32) = (1_440, 900);

const AROUND: Duration = Duration::from_secs(1);

const HELP: &str = "speakeasy show list [--json]
speakeasy show inspect SHOW [--json]
speakeasy show frames SHOW [FOCUS] [--around MS] [--json]
speakeasy show frames SHOW FOCUS --out NEW.png [--most N] [--around MS] [--crop] [--json]
speakeasy show frame SHOW FOCUS --out NEW.png [--crop] [--video [--annotated]] [--json]
speakeasy show keep SHOW [--off] [--json]
speakeasy show export SHOW --out NEW.zip [--json]
speakeasy show video SHOW --out NEW.mp4 [FOCUS [--around MS]] [--captions] [--json]
speakeasy show discard INTERRUPTED_SHOW [--json]
speakeasy show mcp --root PATH [--root PATH ...]
speakeasy show mcp --setup
speakeasy show skill [--install DIR]
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
Source frames are unavailable when their write failed; saved keyframes remain available.
frame --video decodes the retained movie on Windows and macOS, using original session times.
It reports actual decoded time and separate annotation coverage, including recovered video tails.
--annotated adds timed saved ink, pins, and completed actions; reports list omitted annotations.
It requires geometry and annotation coverage at the decoded sample. --crop needs matching geometry.
inspect exits with status 1 for a damaged or unsupported Show.
SHOW is an exact folder-name ID in Speakeasy's Shows folder, or an explicit path.
Use ./ID to address a relative path. No command chooses the latest Show.
keep prevents expiry; --off restores the original seven-day expiry date.
An overdue Show then expires at the next cleanup. Only managed Shows can be kept.
export writes a new ZIP with saved images, narration, retained samples, and a relative reading guide.
It preserves summary edits, excludes recovery audio and unrelated files, and never overwrites.
Exported copies do not expire automatically. Reports print paths and sizes, never private text.
video uses FFmpeg from PATH to export timed annotations and original voice, when available.
Voice expires seven days after recording even for kept Shows; unavailable voice exports silently.
--captions adds a selectable subtitle track from timed narration. Video reports missing coverage.
Focus selects an interval; without FOCUS the entire retained video is exported.
discard permanently removes an explicit interrupted:ID or checkpoint path from list,
including pending audio. Saved Show bundles are unchanged.
Recover interrupted Shows in the Shows window; recovery leaves the clipboard untouched.
mcp serves read-only evidence tools over stdio within explicit roots; --setup prints client snippets.
skill prints the agent reading guide for this installed version.
--install DIR writes DIR/speakeasy-show/SKILL.md; identical content is unchanged and edits are never overwritten.
demo writes a simulated Show and prints its summary.
All commands work without a display, microphone, speech engine or running Speakeasy app.";

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
    Source(source::Request),
    Video(video::Request),
    Mcp(super::mcp::Command),
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
    Skill {
        install: Option<PathBuf>,
    },
    Demo {
        out: Option<PathBuf>,
    },
}

impl Command {
    pub(crate) fn parse(mut args: impl Iterator<Item = OsString>) -> anyhow::Result<Self> {
        let Some(verb) = args.next() else {
            return Ok(Self::Help);
        };
        let verb = verb.to_str().unwrap_or_default();
        match verb {
            "--help" | "-h" => return Ok(Self::Help),
            "frames" => return source::parse(false, args),
            "frame" => return source::parse(true, args),
            "video" => return video::parse(args),
            "mcp" => return super::mcp::Command::parse(args).map(Self::Mcp),
            _ => {},
        }
        let (mut show, mut destination, mut json, mut off) = (None, None, false, false);
        while let Some(arg) = args.next() {
            match (verb, arg.to_str()) {
                (_, Some("--help" | "-h")) => return Ok(Self::Help),
                ("list" | "inspect" | "keep" | "export" | "discard", Some("--json")) if !json => {
                    json = true;
                },
                ("keep", Some("--off")) if !off => off = true,
                ("export", Some("--out")) if destination.is_none() => {
                    destination = Some(path(&mut args, "--out needs a new ZIP file path")?);
                },
                ("demo", Some("--out")) if destination.is_none() => {
                    destination = Some(path(&mut args, "--out needs a new directory")?);
                },
                ("skill", Some("--install")) if destination.is_none() => {
                    destination = Some(path(
                        &mut args,
                        "--install needs the agent's skills directory",
                    )?);
                },
                ("inspect" | "keep" | "export" | "discard", _)
                    if show.is_none() && operand(&arg) =>
                {
                    show = Some(PathBuf::from(arg));
                },
                _ => anyhow::bail!(
                    "Unknown or repeated show argument; speakeasy show --help lists the commands"
                ),
            }
        }
        match verb {
            "list" => Ok(Self::List { json }),
            "inspect" => Ok(Self::Inspect {
                directory: show.context("show inspect needs an exact Show ID or path")?,
                json,
            }),
            "keep" => Ok(Self::Keep {
                directory: show.context("show keep needs an exact Show ID or path")?,
                kept: !off,
                json,
            }),
            "export" => Ok(Self::Export {
                directory: show.context("show export needs an exact Show ID or path")?,
                destination: destination.context("show export needs --out NEW.zip")?,
                json,
            }),
            "discard" => Ok(Self::Discard {
                directory: show
                    .context("show discard needs an explicit interrupted Show ID or path")?,
                json,
            }),
            "skill" => Ok(Self::Skill {
                install: destination,
            }),
            "demo" => Ok(Self::Demo { out: destination }),
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
            Self::Skill { install: None } => write!(out, "{SKILL}")?,
            Self::Skill {
                install: Some(directory),
            } => {
                let path = install_skill(&directory)?;
                writeln!(out, "Show skill available at {}", path.display())?;
            },
            Self::Source(request) => request.write(out)?,
            Self::Video(request) => request.write(out)?,
            Self::Mcp(command) => command.write(out)?,
            Self::Export {
                directory,
                destination,
                json,
            } => {
                let exported = store::export::write(&locate(&directory)?, &destination)?;
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
                let directory = locate(&directory)?;
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

pub(super) fn operand(arg: &OsStr) -> bool {
    !arg.is_empty() && !arg.as_encoded_bytes().starts_with(b"-")
}

pub(super) fn path(
    args: &mut impl Iterator<Item = OsString>,
    missing: &'static str,
) -> anyhow::Result<PathBuf> {
    args.next()
        .filter(|arg| operand(arg))
        .map(PathBuf::from)
        .context(missing)
}

fn value(option: &str, args: &mut impl Iterator<Item = OsString>) -> anyhow::Result<String> {
    args.next()
        .filter(|arg| operand(arg))
        .and_then(|arg| arg.into_string().ok())
        .with_context(|| format!("{option} needs a value"))
}

fn count(option: &str, args: &mut impl Iterator<Item = OsString>) -> anyhow::Result<usize> {
    value(option, args)?
        .parse()
        .ok()
        .filter(|&count| count > 0)
        .with_context(|| format!("{option} needs a positive whole number"))
}

fn focus(option: &str, args: &mut impl Iterator<Item = OsString>) -> anyhow::Result<Query> {
    Ok(match option {
        "--at" => Query::At(query::time(&value(option, args)?)?),
        "--mark" => Query::Mark(count(option, args)?),
        _ => Query::Phrase(value(option, args)?),
    })
}

fn around(args: &mut impl Iterator<Item = OsString>) -> anyhow::Result<Duration> {
    let millis = value("--around", args)?
        .parse()
        .ok()
        .filter(|&millis| millis <= 10_000)
        .context("--around needs milliseconds from 0 to 10000")?;
    Ok(Duration::from_millis(millis))
}

fn matching(focus: Option<Query>, occurrence: Option<usize>) -> anyhow::Result<Option<Query>> {
    match (focus, occurrence) {
        (Some(Query::Phrase(phrase)), Some(occurrence)) => {
            Ok(Some(Query::Match { phrase, occurrence }))
        },
        (_, Some(_)) => anyhow::bail!("--match requires --phrase"),
        (focus, None) => Ok(focus),
    }
}

fn locate(show: &Path) -> anyhow::Result<PathBuf> {
    Ok(address::resolve(show, || {
        store::directory().map_err(io::Error::other)
    })?)
}

fn install_skill(directory: &Path) -> anyhow::Result<PathBuf> {
    let directory = std::path::absolute(directory)?.join("speakeasy-show");
    std::fs::create_dir_all(&directory).context("Cannot create the agent's skill directory")?;
    let path = directory.join("SKILL.md");
    match std::fs::read(&path) {
        Ok(existing) if existing == SKILL.as_bytes() => return Ok(path),
        Ok(_) => anyhow::bail!(
            "The installed Show skill differs; choose a new skills directory or preserve your edits before replacing it"
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(error) => return Err(error).context("Cannot read the existing Show skill"),
    }
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    file.write_all(SKILL.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&path)
        .map_err(|error| error.error)
        .context("Cannot install the Show skill without replacing an existing file")?;
    Ok(path)
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
