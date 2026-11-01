use std::{ffi::OsString, io, path::PathBuf};

use anyhow::{Context as _, ensure};

use crate::show::{
    opencode::{Deliveries, Outcome, Request},
    store::{self, address},
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Sessions {
        json: bool,
    },
    Send {
        directory: PathBuf,
        session: String,
        json: bool,
    },
}

impl Command {
    pub(super) fn parse(
        sending: bool,
        mut args: impl Iterator<Item = OsString>,
    ) -> anyhow::Result<Self> {
        let (mut directory, mut session, mut json) = (None, None, false);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--json") if !json => json = true,
                Some("--session") if sending && session.is_none() => {
                    session = Some(
                        args.next()
                            .and_then(|arg| arg.into_string().ok())
                            .filter(|id| !id.is_empty() && !id.starts_with('-'))
                            .context("--session needs an exact OpenCode session ID")?,
                    );
                },
                _ if sending
                    && directory.is_none()
                    && !arg.is_empty()
                    && !arg.to_string_lossy().starts_with('-') =>
                {
                    directory = Some(PathBuf::from(arg));
                },
                _ => anyhow::bail!(
                    "Unknown or repeated OpenCode argument; use show sessions [--json] or show send SHOW --session ID [--json]"
                ),
            }
        }
        if sending {
            Ok(Self::Send {
                directory: directory.context("show send needs an exact Show ID or path")?,
                session: session.context(
                    "show send needs --session ID; no session is selected automatically",
                )?,
                json,
            })
        } else {
            Ok(Self::Sessions { json })
        }
    }

    pub(super) fn write(self, out: &mut impl io::Write) -> anyhow::Result<()> {
        let mut deliveries = Deliveries::default();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        match self {
            Self::Sessions { json } => {
                let listing = runtime
                    .block_on(deliveries.list()?)
                    .map_err(anyhow::Error::msg)?;
                if json {
                    serde_json::to_writer(&mut *out, &listing)?;
                    writeln!(out)?;
                } else {
                    writeln!(out, "OpenCode at {}", listing.endpoint)?;
                    for session in &listing.sessions {
                        writeln!(
                            out,
                            "{}\t{}\t{}",
                            session.id,
                            session.title.as_deref().unwrap_or("Untitled session"),
                            session.directory
                        )?;
                    }
                    if listing.truncated {
                        writeln!(out, "Only the first 500 sessions are shown.")?;
                    }
                }
            },
            Self::Send {
                directory,
                session,
                json,
            } => {
                let directory =
                    address::resolve(&directory, || store::directory().map_err(io::Error::other))?;
                let outcome = runtime.block_on(deliveries.send(Request {
                    directory,
                    session,
                    expected_endpoint: None,
                })?);
                if json {
                    serde_json::to_writer(&mut *out, &outcome)?;
                    writeln!(out)?;
                } else {
                    writeln!(out, "{}", outcome.note())?;
                }
                ensure!(
                    matches!(outcome, Outcome::Queued { .. }),
                    "OpenCode admission was not confirmed; check the result before sending again."
                );
            },
        }
        Ok(())
    }
}
