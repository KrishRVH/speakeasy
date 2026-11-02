use super::{
    Call, Change, Command, ControlFlow, Directive, ModelState, Owner, Ports, Purpose, Reply,
    Session, Status, now,
};

impl<P: Ports> Owner<P> {
    pub(super) fn command(&mut self, directive: Directive) -> ControlFlow<()> {
        match directive {
            Directive::Control(call) => self.control(call),
            Directive::Preset {
                name,
                config,
                baseline,
                session,
                call,
            } => {
                if !call.is_live() {
                    return ControlFlow::Continue(());
                }
                if self.session.is_some() || self.practice.is_some() {
                    call.respond(Reply::refused(
                        "Finish dictation or practice before choosing a preset.",
                    ));
                    return ControlFlow::Continue(());
                }
                if u64::from(self.latest.id) != session
                    || self.changes.borrow().change_to(&baseline) >= Change::Session
                {
                    call.respond(Reply::refused("Dictation or settings changed while the preset was being checked. Choose it again."));
                    return ControlFlow::Continue(());
                }
                let reload = self.config.change_to(&config) >= Change::Engine;
                self.config = config.into_inner();
                self.preset = Some(name);
                if reload {
                    self.worker.reload(&self.ports, &self.config);
                }
                self.apply_settings();
                call.respond(Reply::ok(self.control_status()));
                ControlFlow::Continue(())
            },
        }
    }

    fn control(&mut self, call: Call) -> ControlFlow<()> {
        if !call.is_live() {
            return ControlFlow::Continue(());
        }
        let refused = match &call.command {
            Command::Status => None,
            Command::Start { style } => {
                if self.session.is_some() || self.practice.is_some() {
                    Some("Finish the current session or practice before starting dictation.")
                } else if !self.desktop_ready || self.worker.model() != ModelState::Ready {
                    Some("Wait for desktop access and the local speech model to be ready.")
                } else {
                    let action = self.gesture.toggle(now());
                    self.handle_action(action)?;
                    if let Some(style) = style
                        && let Some(session) = &mut self.session
                    {
                        session.style = *style;
                        self.latest.style = *style;
                    }
                    self.control_token().is_none().then_some("Dictation could not start because desktop control stopped. Resume Speakeasy before trying again.")
                }
            },
            Command::Stop { session } | Command::Cancel { session }
                if self.control_token().as_ref() != Some(session) =>
            {
                Some(
                    "That ordinary dictation is no longer active. Read status before acting again.",
                )
            },
            Command::Stop { .. } => {
                let action = self.gesture.finish();
                self.handle_action(action)?;
                None
            },
            Command::Cancel { .. } => {
                if let Some(session) = &self.session {
                    session.permit.revoke();
                }
                let action = self.gesture.cancel();
                self.handle_action(action)?;
                None
            },
            Command::PasteLast => {
                if self.session.is_some() || self.practice.is_some() {
                    Some(
                        "Finish the current session or practice before pasting the last dictation.",
                    )
                } else if !self.desktop_ready {
                    Some("Wait for desktop access before pasting the last dictation.")
                } else if self.last_dictation.is_none() {
                    Some("No dictation to paste yet.")
                } else {
                    self.paste_last()?;
                    (!self.session.as_ref().is_some_and(|session| matches!(session.purpose, Purpose::Replay)))
                        .then_some("The last dictation could not be submitted because desktop control stopped. Resume Speakeasy before trying again.")
                }
            },
            Command::Style { .. } | Command::Preset { .. } => {
                Some("Choose a style or saved preset through the desktop command interface.")
            },
        };
        call.respond(refused.map_or_else(|| Reply::ok(self.control_status()), Reply::refused));
        ControlFlow::Continue(())
    }

    fn control_token(&self) -> Option<String> {
        self.session
            .as_ref()
            .filter(|session| !session.is_show() && matches!(session.purpose, Purpose::Dictation))
            .map(|session| format!("{:016x}-{}", self.incarnation, u64::from(session.id)))
    }

    fn control_status(&self) -> Status {
        Status {
            version: env!("CARGO_PKG_VERSION").into(),
            service: "running".into(),
            phase: self
                .session
                .as_ref()
                .map_or_else(|| self.outcome.phase(), Session::phase)
                .name()
                .into(),
            model: self.worker.model().name().into(),
            desktop_ready: self.desktop_ready,
            session: self.control_token(),
            style: *self.styles.borrow(),
            preset: self.preset.clone(),
            last_dictation: self.last_dictation.is_some(),
        }
    }
}
