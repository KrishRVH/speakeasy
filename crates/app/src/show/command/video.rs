use std::{ffi::OsString, io, path::PathBuf, time::Duration};

use anyhow::Context as _;
use speakeasy_show::{Moment, Span};

use super::{
    output,
    query::{self, Query},
};
use crate::show::store::{self, address, recall, video};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Export(Request),
}

#[derive(PartialEq, Eq)]
pub(crate) struct Request {
    directory: PathBuf,
    destination: PathBuf,
    focus: Option<Query>,
    around: u64,
    captions: bool,
    json: bool,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("VideoRequest").finish_non_exhaustive()
    }
}

impl Command {
    pub(super) fn parse(mut args: impl Iterator<Item = OsString>) -> anyhow::Result<Self> {
        let (mut directory, mut destination, mut focus, mut occurrence, mut around) =
            (None, None, None, None, None);
        let (mut captions, mut json) = (false, false);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--help" | "-h") => return Ok(Self::Help),
                Some("--json") if !json => json = true,
                Some("--captions") if !captions => captions = true,
                Some("--out") if destination.is_none() => {
                    destination = Some(PathBuf::from(
                        args.next()
                            .filter(|arg| {
                                !arg.is_empty() && !arg.to_string_lossy().starts_with('-')
                            })
                            .context("--out needs a new MP4 path")?,
                    ));
                },
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
                Some("--around") if around.is_none() => {
                    let ms = value(&mut args)?
                        .parse::<u64>()
                        .context("--around needs milliseconds from 0 to 10000")?;
                    anyhow::ensure!(ms <= 10_000, "--around accepts at most 10000 milliseconds");
                    around = Some(ms);
                },
                _ if directory.is_none()
                    && !arg.is_empty()
                    && !arg.to_string_lossy().starts_with('-') =>
                {
                    directory = Some(PathBuf::from(arg));
                },
                _ => anyhow::bail!("Invalid video option; use speakeasy show video --help"),
            }
        }
        if let Some(occurrence) = occurrence {
            focus = Some(match focus {
                Some(Query::Phrase(phrase)) => Query::Match { phrase, occurrence },
                _ => anyhow::bail!("--match requires --phrase"),
            });
        }
        anyhow::ensure!(
            around.is_none() || focus.is_some(),
            "--around requires a focus"
        );
        Ok(Self::Export(Request {
            directory: directory.context("video requires an explicit Show")?,
            destination: destination.context("video requires --out NEW.mp4")?,
            focus,
            around: around.unwrap_or(1000),
            captions,
            json,
        }))
    }

    pub(super) fn write(self, out: &mut impl io::Write) -> anyhow::Result<()> {
        let Self::Export(request) = self else {
            return writeln!(out, "{}", super::HELP).map_err(Into::into);
        };
        let directory = address::resolve(&request.directory, || {
            store::directory().map_err(io::Error::other)
        })?;
        let span = request
            .focus
            .map(|focus| {
                let manifest = recall::Entry::open(&directory)?.show?.manifest;
                let span = focus
                    .resolve(&manifest)?
                    .selected
                    .context("Several phrases match; select one with --match K before exporting")?;
                Ok::<_, anyhow::Error>(Span::new(
                    Moment::from_millis(span.start().as_millis().saturating_sub(request.around)),
                    span.end()
                        .saturating_add(Duration::from_millis(request.around)),
                ))
            })
            .transpose()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(export(
            directory,
            request.destination,
            video::Options {
                span,
                captions: request.captions,
            },
        ))?;
        if request.json {
            output::json(&result, out)?;
        } else {
            writeln!(out, "{}", result.note())?;
        }
        Ok(())
    }
}

async fn export(
    directory: PathBuf,
    destination: PathBuf,
    options: video::Options,
) -> anyhow::Result<video::Exported> {
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    if let std::task::Poll::Ready(result) = futures_util::poll!(&mut interrupt) {
        result.context("Cannot listen for video export cancellation")?;
        anyhow::bail!("Video export cancelled");
    }
    let (cancel, receiver) = async_channel::bounded::<()>(1);
    let mut work = tokio::task::spawn_blocking(move || {
        video::write(&directory, &destination, options, || receiver.is_closed())
    });
    tokio::select! {
        result = &mut work => result.map_err(|_private_panic| anyhow::anyhow!("Video export stopped"))?,
        signal = &mut interrupt => {
            drop(cancel);
            let result = work.await.map_err(|_private_panic| anyhow::anyhow!("Video export stopped"))?;
            signal.context("Cannot listen for video export cancellation")?;
            result
        },
    }
}

fn value(args: &mut impl Iterator<Item = OsString>) -> anyhow::Result<String> {
    args.next()
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty() && !value.starts_with('-'))
        .context("The video option needs a value")
}

fn positive(value: &str) -> anyhow::Result<usize> {
    value
        .parse()
        .ok()
        .filter(|&value| value > 0)
        .context("Mark and match numbers start at 1")
}

#[cfg(test)]
mod tests;
