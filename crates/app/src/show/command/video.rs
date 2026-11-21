use std::{ffi::OsString, io, path::PathBuf, time::Duration};

use anyhow::Context as _;
use speakeasy_show::{Manifest, Span};

use super::{AROUND, output};
use crate::show::{evidence::query::Query, store::video};

#[derive(PartialEq, Eq)]
pub(crate) struct Request {
    directory: PathBuf,
    destination: PathBuf,
    focus: Option<Query>,
    around: Duration,
    captions: bool,
    json: bool,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("VideoRequest").finish_non_exhaustive()
    }
}

pub(super) fn parse(mut args: impl Iterator<Item = OsString>) -> anyhow::Result<super::Command> {
    let (mut directory, mut destination, mut focus, mut occurrence, mut around) =
        (None, None, None, None, None);
    let (mut captions, mut json) = (false, false);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => return Ok(super::Command::Help),
            Some("--json") if !json => json = true,
            Some("--captions") if !captions => captions = true,
            Some("--out") if destination.is_none() => {
                destination = Some(super::path(&mut args, "--out needs a new MP4 path")?);
            },
            Some(option @ ("--at" | "--mark" | "--phrase")) if focus.is_none() => {
                focus = Some(super::focus(option, &mut args)?);
            },
            Some("--match") if occurrence.is_none() => {
                occurrence = Some(super::count("--match", &mut args)?);
            },
            Some("--around") if around.is_none() => around = Some(super::around(&mut args)?),
            _ if directory.is_none() && super::operand(&arg) => {
                directory = Some(PathBuf::from(arg));
            },
            _ => anyhow::bail!("Invalid video option; use speakeasy show video --help"),
        }
    }
    let focus = super::matching(focus, occurrence)?;
    anyhow::ensure!(
        around.is_none() || focus.is_some(),
        "--around requires a focus"
    );
    Ok(super::Command::Video(Request {
        directory: directory.context("video requires an explicit Show")?,
        destination: destination.context("video requires --out NEW.mp4")?,
        focus,
        around: around.unwrap_or(AROUND),
        captions,
        json,
    }))
}

impl Request {
    pub(super) fn write(self, out: &mut impl io::Write) -> anyhow::Result<()> {
        let directory = super::locate(&self.directory)?;
        let (focus, around) = (self.focus, self.around);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let exported = runtime.block_on(export(
            directory,
            self.destination,
            video::Options {
                captions: self.captions,
            },
            move |manifest| {
                focus
                    .map(|focus| widened(manifest, &focus, around))
                    .transpose()
            },
        ))?;
        if self.json {
            output::json(&exported, out)?;
        } else {
            writeln!(out, "{}", exported.note())?;
        }
        Ok(())
    }
}

fn widened(manifest: &Manifest, focus: &Query, around: Duration) -> anyhow::Result<Span> {
    let span = if let Query::At(at) = focus {
        Span::at(*at)
    } else {
        focus
            .resolve(manifest)?
            .selected
            .context("Several phrases match; select one with --match K before exporting")?
    };
    Ok(Span::new(
        span.start().saturating_sub(around),
        span.end().saturating_add(around),
    ))
}

async fn export(
    directory: PathBuf,
    destination: PathBuf,
    options: video::Options,
    clip: impl FnOnce(&Manifest) -> anyhow::Result<Option<Span>> + Send + 'static,
) -> anyhow::Result<video::Exported> {
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    if let std::task::Poll::Ready(result) = futures_util::poll!(&mut interrupt) {
        result.context("Cannot listen for video export cancellation")?;
        anyhow::bail!(video::CANCELLED);
    }
    let (cancel, receiver) = async_channel::bounded::<()>(1);
    let mut work = tokio::task::spawn_blocking(move || {
        video::write(&directory, &destination, options, clip, || {
            receiver.is_closed()
        })
    });
    let stopped = |_private_panic| anyhow::anyhow!("Video export stopped");
    tokio::select! {
        result = &mut work => result.map_err(stopped)?,
        signal = &mut interrupt => {
            drop(cancel);
            let result = work.await.map_err(stopped)?;
            signal.context("Cannot listen for video export cancellation")?;
            result.map_err(|_cancelled| anyhow::anyhow!(video::CANCELLED))
        },
    }
}

#[cfg(test)]
mod tests;
