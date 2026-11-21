use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow, bail, ensure};
use async_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::{paths, transcript::style::Style};

const VERSION: u8 = 1;
const TIMEOUT: Duration = Duration::from_secs(2);
const PRINT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Command {
    Status,
    Start { style: Option<Style> },
    Capture { style: Option<Style> },
    Stop { session: String },
    Print { session: String },
    Cancel { session: String },
    PasteLast,
    Style { name: Style },
    Preset { name: String },
}

impl Command {
    const fn timeout(&self) -> Duration {
        match self {
            Self::Print { .. } => PRINT_TIMEOUT,
            Self::Status
            | Self::Start { .. }
            | Self::Capture { .. }
            | Self::Stop { .. }
            | Self::Cancel { .. }
            | Self::PasteLast
            | Self::Style { .. }
            | Self::Preset { .. } => TIMEOUT,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Status {
    pub version: String,
    pub service: String,
    pub phase: String,
    pub model: String,
    pub desktop_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub style: Style,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    pub last_dictation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Disposition {
    Ok,
    Refused,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Reply {
    v: u8,
    pub result: Disposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl Reply {
    pub(crate) const fn accepted() -> Self {
        Self {
            v: VERSION,
            result: Disposition::Ok,
            reason: None,
            session: None,
            status: None,
            text: None,
        }
    }

    pub(crate) fn ok(status: Status) -> Self {
        Self {
            session: status.session.clone(),
            status: Some(status),
            ..Self::accepted()
        }
    }

    pub(crate) fn printed(text: String) -> Self {
        Self {
            text: Some(text),
            ..Self::accepted()
        }
    }

    pub(crate) fn refused(reason: impl Into<String>) -> Self {
        Self {
            result: Disposition::Refused,
            reason: Some(reason.into()),
            ..Self::accepted()
        }
    }

    pub(crate) fn unknown(reason: impl Into<String>) -> Self {
        Self {
            result: Disposition::Unknown,
            reason: Some(reason.into()),
            ..Self::accepted()
        }
    }

    pub(crate) const fn exit_code(&self, delivered: bool) -> u8 {
        match (self.result, delivered) {
            (Disposition::Ok, true) => 0,
            (Disposition::Refused, _) => 1,
            (Disposition::Ok, false) | (Disposition::Unknown, _) => 2,
        }
    }

    pub(crate) fn write(&self, out: &mut impl Write) -> anyhow::Result<()> {
        serde_json::to_writer(&mut *out, self)?;
        writeln!(out)?;
        Ok(())
    }
}

pub(crate) struct Call {
    pub command: Command,
    reply: Sender<Reply>,
    deadline: Instant,
}

impl Call {
    pub(crate) fn new(command: Command) -> (Self, Receiver<Reply>) {
        let now = Instant::now();
        let deadline = now.checked_add(command.timeout()).unwrap_or(now);
        Self::until(command, deadline)
    }

    fn until(command: Command, deadline: Instant) -> (Self, Receiver<Reply>) {
        let (reply, receipt) = async_channel::bounded(1);
        (
            Self {
                command,
                reply,
                deadline,
            },
            receipt,
        )
    }

    pub(crate) const fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) fn is_live(&self) -> bool {
        !self.reply.is_closed() && Instant::now() < self.deadline
    }

    pub(crate) fn forwarded(&self) -> (Self, Receiver<Reply>) {
        Self::until(self.command.clone(), self.deadline)
    }

    pub(crate) fn respond(self, reply: Reply) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A caller that disconnected or expired no longer receives its owner's result"
        )]
        let _ = self.reply.try_send(reply);
    }
}

pub(crate) struct Invocation {
    pub settings: Option<PathBuf>,
    pub command: Command,
}

impl Invocation {
    pub(crate) fn recognizes(argument: &OsStr) -> bool {
        matches!(
            argument.to_str(),
            Some("status" | "dictation" | "paste-last" | "style" | "preset")
        )
    }

    pub(crate) fn parse(mut arguments: impl Iterator<Item = OsString>) -> anyhow::Result<Self> {
        let mut settings = None;
        let mut words = Vec::new();
        while let Some(argument) = arguments.next() {
            if argument == "--config" {
                ensure!(settings.is_none(), "Use --config only once.");
                settings = Some(paths::settings_argument(arguments.next())?);
            } else {
                words.push(argument.into_string().map_err(|_error| {
                    anyhow!("Control command arguments must be Unicode text.")
                })?);
            }
        }
        let words: Vec<_> = words.iter().map(String::as_str).collect();
        let command = match words.as_slice() {
            ["status"] => Command::Status,
            ["dictation", "start"] => Command::Start { style: None },
            ["dictation", "start", "--style", name] => Command::Start {
                style: Some(style(name)?),
            },
            ["dictation", "start", "--print"] => Command::Capture { style: None },
            ["dictation", "start", "--print", "--style", name]
            | ["dictation", "start", "--style", name, "--print"] => Command::Capture {
                style: Some(style(name)?),
            },
            ["dictation", "stop", token] => Command::Stop {
                session: session(token)?,
            },
            ["dictation", "stop", token, "--print"] => Command::Print {
                session: session(token)?,
            },
            ["dictation", "cancel", token] => Command::Cancel {
                session: session(token)?,
            },
            ["paste-last"] => Command::PasteLast,
            ["style", name] => Command::Style { name: style(name)? },
            ["preset", name] => Command::Preset {
                name: (*name).into(),
            },
            _ => bail!(
                "Use status, dictation start [--print] [--style STYLE], dictation stop TOKEN [--print], \
                 dictation cancel TOKEN, \
                 paste-last, style STYLE, or preset NAME."
            ),
        };
        Ok(Self { settings, command })
    }
}

fn style(name: &str) -> anyhow::Result<Style> {
    Style::parse(name).with_context(|| {
        format!(
            "Unknown dictation style. Styles: {}.",
            Style::ALL.map(Style::name).join(", ")
        )
    })
}

fn session(token: &str) -> anyhow::Result<String> {
    ensure!(
        !token.starts_with('-'),
        "Use the exact session token that dictation start returned."
    );
    Ok(token.into())
}

#[cfg(test)]
mod tests;
