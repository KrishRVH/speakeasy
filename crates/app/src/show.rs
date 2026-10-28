//! Shows: screen recordings with narration and ink, written as bundles a coding agent reads, and
//! the `show demo` command that writes a scripted one.

mod overlay;
mod publish;
mod recorder;
mod store;

use std::{
    io::{self, Write},
    path::PathBuf,
    time::SystemTime,
};

use anyhow::Context as _;
use speakeasy_show::{Demo, Typeface};

pub(crate) use self::{
    overlay::{Canvas, attend as attend_overlay},
    publish::publish,
    recorder::ScreenRecorder,
    store::attend as attend_retention,
};

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
        let directory = match out {
            Some(out) => {
                let out = std::path::absolute(out)?;
                anyhow::ensure!(
                    !out.exists(),
                    "{} already exists; choose a new directory",
                    out.display()
                );
                out
            },
            None => store::fresh_directory(&store::private_directory()?, SystemTime::now())?,
        };
        let bundle = Demo::new(Typeface::parse(crate::BRAND_FONT_FILE)?).bundle(DEMO_SIZE)?;
        let summary = store::stage(&directory, bundle)?.persist()?;
        write!(io::stdout().lock(), "{summary}")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
