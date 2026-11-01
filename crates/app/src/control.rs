use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail, ensure};
use async_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::transcript::style::Style;

pub(crate) const VERSION: u8 = 1;
pub(crate) const MAX_BYTES: usize = 4096;
pub(crate) const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Command {
    Status,
    Start {
        #[serde(skip_serializing_if = "Option::is_none")]
        style: Option<Style>,
    },
    Stop {
        session: String,
    },
    Cancel {
        session: String,
    },
    PasteLast,
    Style {
        name: Style,
    },
    Preset {
        name: String,
    },
}

impl fmt::Debug for Command {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Command").finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

impl fmt::Debug for Status {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Status").finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Disposition {
    Ok,
    Refused,
    Unknown,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reply {
    v: u8,
    pub result: Disposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
}

impl fmt::Debug for Reply {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Reply")
            .field("result", &self.result)
            .finish_non_exhaustive()
    }
}

impl Reply {
    pub(crate) const fn accepted() -> Self {
        Self {
            v: VERSION,
            result: Disposition::Ok,
            reason: None,
            session: None,
            status: None,
        }
    }

    pub(crate) fn ok(status: Status) -> Self {
        Self {
            v: VERSION,
            result: Disposition::Ok,
            reason: None,
            session: status.session.clone(),
            status: Some(status),
        }
    }

    pub(crate) fn refused(reason: impl Into<String>) -> Self {
        Self::failure(Disposition::Refused, reason.into())
    }

    pub(crate) fn unknown(reason: impl Into<String>) -> Self {
        Self::failure(Disposition::Unknown, reason.into())
    }

    fn failure(result: Disposition, reason: String) -> Self {
        Self {
            v: VERSION,
            result,
            reason: Some(reason),
            session: None,
            status: None,
        }
    }

    pub(crate) const fn exit_code(&self) -> u8 {
        match self.result {
            Disposition::Ok => 0,
            Disposition::Refused => 1,
            Disposition::Unknown => 2,
        }
    }

    pub(crate) fn write(&self, out: &mut impl Write) -> anyhow::Result<()> {
        let json = self.encode().map_err(anyhow::Error::msg)?;
        out.write_all(&json)?;
        writeln!(out)?;
        Ok(())
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let bytes =
            serde_json::to_vec(self).map_err(|_error| "Cannot encode the control reply.")?;
        if bytes.len() > MAX_BYTES {
            return Err("The control reply exceeds its size limit.");
        }
        Ok(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let reply: Self =
            serde_json::from_slice(bytes).map_err(|_error| "Invalid control reply.")?;
        if reply.v != VERSION {
            return Err("Unsupported control reply version.");
        }
        Ok(reply)
    }
}

pub(crate) struct Call {
    pub command: Command,
    pub reply: Sender<Reply>,
    pub deadline: Instant,
    caller: Option<Sender<Reply>>,
}

impl fmt::Debug for Call {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Call").finish_non_exhaustive()
    }
}

impl Call {
    pub(crate) fn new(command: Command) -> (Self, Receiver<Reply>) {
        let (reply, receipt) = async_channel::bounded(1);
        let now = Instant::now();
        (
            Self {
                command,
                reply,
                deadline: now.checked_add(TIMEOUT).unwrap_or(now),
                caller: None,
            },
            receipt,
        )
    }

    pub(crate) fn is_live(&self) -> bool {
        !self.reply.is_closed()
            && self
                .caller
                .as_ref()
                .is_none_or(|caller| !caller.is_closed())
            && Instant::now() < self.deadline
    }

    pub(crate) fn forwarded(&self) -> (Self, Receiver<Reply>) {
        let (mut call, receipt) = Self::new(self.command.clone());
        call.deadline = self.deadline;
        call.caller = Some(self.caller.as_ref().unwrap_or(&self.reply).clone());
        (call, receipt)
    }

    pub(crate) fn respond(self, reply: Reply) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A caller that disconnected or expired no longer receives its owner's result"
        )]
        let _ = self.reply.try_send(reply);
    }
}

#[derive(Serialize)]
struct Outgoing<'command> {
    v: u8,
    #[serde(flatten)]
    command: &'command Command,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Incoming {
    v: u8,
    op: String,
    session: Option<String>,
    name: Option<String>,
    style: Option<Style>,
}

impl Command {
    pub(super) fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let valid = match self {
            Self::Stop { session } | Self::Cancel { session } => valid_token(session),
            Self::Preset { name } => valid_name(name),
            Self::Status | Self::Start { .. } | Self::PasteLast | Self::Style { .. } => true,
        };
        if !valid {
            return Err("Invalid control command arguments.");
        }
        let bytes = serde_json::to_vec(&Outgoing {
            v: VERSION,
            command: self,
        })
        .map_err(|_error| "Cannot encode the control command.")?;
        if bytes.len() > MAX_BYTES {
            return Err("The control command exceeds its size limit.");
        }
        Ok(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let request: Incoming =
            serde_json::from_slice(bytes).map_err(|_error| "Invalid control command.")?;
        if request.v != VERSION {
            return Err("Unsupported control version. Restart Speakeasy to use this command.");
        }
        match (
            request.op.as_str(),
            request.session,
            request.name,
            request.style,
        ) {
            ("status", None, None, None) => Ok(Self::Status),
            ("start", None, None, style) => Ok(Self::Start { style }),
            ("stop", Some(session), None, None) if valid_token(&session) => {
                Ok(Self::Stop { session })
            },
            ("cancel", Some(session), None, None) if valid_token(&session) => {
                Ok(Self::Cancel { session })
            },
            ("paste_last", None, None, None) => Ok(Self::PasteLast),
            ("style", None, Some(name), None) => Style::parse(&name)
                .map(|name| Self::Style { name })
                .ok_or("Unknown dictation style."),
            ("preset", None, Some(name), None) if valid_name(&name) => Ok(Self::Preset { name }),
            _ => Err("Invalid control command arguments."),
        }
    }
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 64
        && !token
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && name.chars().count() <= 48
        && !name.chars().any(char::is_control)
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

    pub(crate) fn parse(arguments: impl Iterator<Item = OsString>) -> anyhow::Result<Self> {
        let mut settings = None;
        let mut words = Vec::new();
        let mut arguments = arguments;
        while let Some(argument) = arguments.next() {
            if argument == "--config" {
                ensure!(settings.is_none(), "Use --config only once.");
                let path = arguments.next().map(PathBuf::from);
                settings = Some(
                    path.filter(|path| path.file_name().is_some())
                        .context("--config needs the path of a settings file.")?,
                );
            } else {
                words.push(argument.into_string().map_err(|_error| {
                    anyhow::anyhow!("Control command arguments must be Unicode text.")
                })?);
            }
        }
        let words: Vec<_> = words.iter().map(String::as_str).collect();
        let command = match words.as_slice() {
            ["status"] => Command::Status,
            ["dictation", "start"] => Command::Start { style: None },
            ["dictation", "start", "--style", name] => Command::Start {
                style: Some(Style::parse(name).context("Unknown dictation style.")?),
            },
            ["dictation", "stop", token] if valid_token(token) => Command::Stop {
                session: (*token).into(),
            },
            ["dictation", "cancel", token] if valid_token(token) => Command::Cancel {
                session: (*token).into(),
            },
            ["paste-last"] => Command::PasteLast,
            ["style", name] => Command::Style {
                name: Style::parse(name).context("Unknown dictation style.")?,
            },
            ["preset", name] if valid_name(name) => Command::Preset {
                name: (*name).into(),
            },
            _ => bail!(
                "Use status, dictation start [--style NAME], dictation stop TOKEN, dictation cancel TOKEN, paste-last, style NAME or preset NAME."
            ),
        };
        Ok(Self { settings, command })
    }
}

#[cfg(test)]
mod tests;
