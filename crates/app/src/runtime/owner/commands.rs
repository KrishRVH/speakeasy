use speakeasy_core::machine::{self, Event, Refusal, SessionId};

use super::World;
use crate::{
    config::Change,
    control::{Command, Reply, Status},
    runtime::{Directive, native::Native, ports::Ports},
    status::ServiceState,
};

impl<P: Ports> World<P> {
    pub(super) fn direct(&mut self, directive: Directive, idle: bool) -> Option<Event<Native>> {
        match directive {
            Directive::Control(call)
                if matches!(
                    call.command,
                    Command::Capture { .. } | Command::Print { .. }
                ) && !self.baseline.print_to_scripts =>
            {
                call.respond(Reply::refused(SCRIPTS_CANNOT_READ));
                None
            },
            Directive::Control(call) if call.is_live() => {
                let command = match &call.command {
                    Command::Status => machine::Command::Status,
                    Command::Start { style } => machine::Command::Start(*style),
                    Command::Capture { style } => machine::Command::Capture(*style),
                    Command::Stop { session } => machine::Command::Stop(self.session(session)),
                    Command::Print { session } => machine::Command::Print(self.session(session)),
                    Command::Cancel { session } => machine::Command::Cancel(self.session(session)),
                    Command::PasteLast => machine::Command::PasteLast,
                    Command::Style { .. } | Command::Preset { .. } => machine::Command::Unsupported,
                };
                Some(Event::Control { call, command })
            },
            Directive::Preset {
                name,
                config,
                baseline,
                session,
                call,
            } if call.is_live() => Some(Event::Preset {
                call,
                name,
                config: config.into_inner(),
                session: SessionId::new(session),
                changed: self.baseline.change_to(&baseline) >= Change::Session,
            }),
            Directive::Adopt { config, answer } if idle && answer.is_live() => {
                answer.answer(Ok(()));
                self.baseline.clone_from(&config);
                Some(Event::Configure(*config))
            },
            Directive::Revert(config) => {
                self.baseline = *config;
                self.reverting = true;
                None
            },
            Directive::Apply(config) => {
                self.baseline.clone_from(&config);
                self.reverting = false;
                Some(Event::Configure(*config))
            },
            Directive::Adopt { answer, .. } => {
                answer.answer(Err(refusal(Refusal::PresetBusy)));
                None
            },
            Directive::Control(_) | Directive::Preset { .. } => None,
        }
    }

    pub(super) fn status(&self, status: machine::Status<Native>) -> Status {
        Status {
            version: env!("CARGO_PKG_VERSION").into(),
            service: ServiceState::Running.name().into(),
            phase: status.phase.name().into(),
            model: status.model.name().into(),
            desktop_ready: status.desktop_ready,
            session: status.session.map(|session| self.token(session)),
            style: *self.styles.borrow(),
            preset: status.preset,
            last_dictation: status.last_dictation,
        }
    }

    fn token(&self, session: SessionId) -> String {
        format!("{:016x}-{}", self.incarnation, u64::from(session))
    }

    fn session(&self, token: &str) -> Option<SessionId> {
        let (_, id) = token.rsplit_once('-')?;
        let session = SessionId::new(id.parse().ok()?);
        (self.token(session) == token).then_some(session)
    }
}

const SCRIPTS_CANNOT_READ: &str =
    "Turn on “Scripts can read their dictations” in Settings › Dictation › Writing to use --print.";

pub(in crate::runtime) const fn refusal(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::Busy => "Finish the current session or practice before starting dictation.",
        Refusal::NotReady => "Wait for desktop access and the local speech model to be ready.",
        Refusal::StartStopped => {
            "Dictation could not start because desktop control stopped. Resume Speakeasy before trying again."
        },
        Refusal::NotActive => {
            "That ordinary dictation is no longer active. Read status before acting again."
        },
        Refusal::PasteBusy => {
            "Finish the current session or practice before pasting the last dictation."
        },
        Refusal::PasteNotReady => "Wait for desktop access before pasting the last dictation.",
        Refusal::NothingToPaste => "No dictation to paste yet.",
        Refusal::PasteStopped => {
            "The last dictation could not be submitted because desktop control stopped. Resume Speakeasy before trying again."
        },
        Refusal::Unsupported => {
            "Choose a style or saved preset through the desktop command interface."
        },
        Refusal::PresetBusy => "Finish dictation or practice before choosing a preset.",
        Refusal::PresetStale => {
            "Dictation or settings changed while the preset was being checked. Choose it again."
        },
        Refusal::NotCaptured => {
            "Only a dictation started with dictation start --print can be printed, and only once."
        },
        Refusal::Unheard => {
            "The dictation ended without words to print: it was empty, cancelled, or failed."
        },
    }
}
