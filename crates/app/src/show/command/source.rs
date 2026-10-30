//! Explicit source inspection and PNG export. Output contains coverage and paths, never pixels.

use std::{ffi::OsString, io, path::PathBuf};

use anyhow::Context as _;
use serde::Serialize;
use speakeasy_show::Moment;

use crate::show::store::{self, address, source};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Frames {
        directory: PathBuf,
        json: bool,
    },
    Frame {
        directory: PathBuf,
        at: Moment,
        destination: PathBuf,
        json: bool,
    },
}

impl Command {
    pub(super) fn parse(
        export: bool,
        mut args: impl Iterator<Item = OsString>,
    ) -> anyhow::Result<Self> {
        let (mut directory, mut at, mut destination, mut json) = (None, None, None, false);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--help" | "-h") => return Ok(Self::Help),
                Some("--json") if !json => json = true,
                Some("--at") if export && at.is_none() => {
                    at = Some(time(
                        args.next()
                            .as_deref()
                            .and_then(std::ffi::OsStr::to_str)
                            .context("--at needs a time such as 0:12.500")?,
                    )?);
                },
                Some("--out") if export && destination.is_none() => {
                    destination = Some(PathBuf::from(
                        args.next()
                            .filter(|arg| !arg.is_empty())
                            .context("--out needs a new PNG path")?,
                    ));
                },
                _ if directory.is_none()
                    && !arg.is_empty()
                    && !arg.to_string_lossy().starts_with('-') =>
                {
                    directory = Some(PathBuf::from(arg));
                },
                _ => {
                    anyhow::bail!("Unknown or repeated source argument; see speakeasy show --help")
                },
            }
        }
        let directory = directory.context("Source inspection needs an exact Show ID or path")?;
        Ok(if export {
            Self::Frame {
                directory,
                at: at.context("show frame needs --at M:SS.mmm")?,
                destination: destination.context("show frame needs --out NEW.png")?,
                json,
            }
        } else {
            Self::Frames { directory, json }
        })
    }

    pub(super) fn write(self, out: &mut impl io::Write) -> anyhow::Result<()> {
        let (path, json) = match &self {
            Self::Help => return super::Command::Help.write(out),
            Self::Frames { directory, json }
            | Self::Frame {
                directory, json, ..
            } => (directory, *json),
        };
        let directory = address::resolve(path, || store::directory().map_err(io::Error::other))?;
        anyhow::ensure!(
            source::size(&directory)?.is_some(),
            "This Show has no retained source frames. Its selected keyframes may still be available through show inspect."
        );
        let index = source::inspect(&directory)?;
        match self {
            Self::Help => {},
            Self::Frames { .. } => {
                let coverage = Coverage {
                    path: directory.join(source::FILE),
                    duration_ms: index.duration().as_millis(),
                    arrivals: index.arrivals(),
                    retained: index.moments().len(),
                    stride: index.stride(),
                    longest_gap_ms: source::longest_gap(&index),
                    bytes: index.bytes(),
                    times_ms: index.moments().iter().map(|at| at.as_millis()).collect(),
                };
                if json {
                    serde_json::to_writer_pretty(&mut *out, &coverage)?;
                    writeln!(out)?;
                } else {
                    writeln!(
                        out,
                        "{} retained frames from {} arrivals · stride {} · longest gap {} ms · {} bytes",
                        coverage.retained,
                        coverage.arrivals,
                        coverage.stride,
                        coverage.longest_gap_ms,
                        coverage.bytes
                    )?;
                    for at in index.moments() {
                        writeln!(out, "{at} ({} ms)", at.as_millis())?;
                    }
                }
            },
            Self::Frame {
                at, destination, ..
            } => {
                let served = source::frame(&directory, &index, at)?;
                let destination = std::path::absolute(destination)?;
                source::export(&destination, &served.image.encode_png()?).with_context(|| {
                    format!(
                        "Cannot create {}; choose a new writable PNG path",
                        destination.display()
                    )
                })?;
                if json {
                    serde_json::to_writer_pretty(
                        &mut *out,
                        &Export {
                            path: destination,
                            requested_ms: at.as_millis(),
                            sampled_ms: served.at.as_millis(),
                        },
                    )?;
                    writeln!(out)?;
                } else {
                    writeln!(
                        out,
                        "{} · requested {} ms · sampled {} ms",
                        destination.display(),
                        at.as_millis(),
                        served.at.as_millis()
                    )?;
                }
            },
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Export {
    path: PathBuf,
    requested_ms: u64,
    sampled_ms: u64,
}

fn time(value: &str) -> anyhow::Result<Moment> {
    let number = |part: &str| -> anyhow::Result<u64> {
        anyhow::ensure!(
            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()),
            "Time must be seconds or M:SS, with up to three decimal places"
        );
        Ok(part.parse()?)
    };
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    anyhow::ensure!(
        fraction.len() <= 3 && (!value.contains('.') || !fraction.is_empty()),
        "Time accepts one to three decimal places"
    );
    let seconds = if let Some((minutes, seconds)) = whole.split_once(':') {
        let seconds = number(seconds)?;
        anyhow::ensure!(seconds < 60, "Seconds after ':' must be less than 60");
        number(minutes)?
            .checked_mul(60)
            .and_then(|minutes| minutes.checked_add(seconds))
    } else {
        Some(number(whole)?)
    }
    .context("Time is too large")?;
    let millis = if fraction.is_empty() {
        0
    } else {
        number(fraction)?
            .saturating_mul(10_u64.pow(3_u32.saturating_sub(u32::try_from(fraction.len())?)))
    };
    Ok(Moment::from_millis(
        seconds
            .checked_mul(1_000)
            .and_then(|seconds| seconds.checked_add(millis))
            .context("Time is too large")?,
    ))
}

#[cfg(test)]
mod tests;
