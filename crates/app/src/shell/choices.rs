use async_channel::Receiver;
use gpui::{App, Task};

use super::Services;
use crate::{
    config::{Change, Config, ValidConfig, presets::Preset},
    control::{Call, Command, Disposition, Reply, Status},
    runtime::{Adopted, Runtime},
};

const UNACKNOWLEDGED: &str =
    "Preset selection stopped before acknowledging. Read status before choosing again.";

const REPLACED: &str =
    "Newer settings replaced the preset before it was confirmed. Choose it again.";

const USE_UNACKNOWLEDGED: &str =
    "Dictation stopped before the preset was applied. Resume, then use it again.";

const USE_BUSY: &str = "Finish the current session or preset selection before using a preset.";

pub(super) const USE_OVERTAKEN: &str = "Speech settings changed before the preset was ready, so it was not used. Use it again to apply it.";

const USE_UNAVAILABLE: &str =
    "This preset's engine or model is unavailable. Choose them again, then update this preset.";

pub(super) type Used = Result<ValidConfig, &'static str>;

enum Admission {
    Ready(ValidConfig),
    Waiting(ValidConfig, u64, Receiver<Adopted>),
}

pub(super) struct Selected {
    pub preset: Preset,
    pub config: ValidConfig,
}

struct Selection {
    request: u64,
    session: u64,
    preset: Preset,
    baseline: Config,
}

#[derive(Default)]
pub(super) struct Choices {
    pub selected: Option<Selected>,
    sent: u64,
    answered: u64,
    honored: u64,
    choosing: Option<Task<()>>,
    using: Option<Task<()>>,
}

impl Choices {
    pub(super) fn cancel(&mut self) {
        self.choosing = None;
        self.using = None;
    }

    pub(super) fn forget(&mut self) {
        self.cancel();
        self.supersede();
        self.selected = None;
    }

    pub(super) fn supersede(&mut self) {
        let request = self.request();
        self.answered = request;
        self.honored = request;
    }

    fn request(&mut self) -> u64 {
        self.sent = self.sent.wrapping_add(1);
        self.sent
    }

    fn answer(&mut self, request: u64, accepted: bool) -> bool {
        self.answered = self.answered.max(request);
        let honored = accepted && request > self.honored;
        if honored {
            self.honored = request;
        }
        honored
    }

    const fn is_settled(&self) -> bool {
        self.answered == self.sent
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
    pub(super) const fn awaits_owner(&self) -> bool {
        !self.is_settled() && self.choosing.is_none()
    }
}

impl Services {
    pub(crate) fn preset_name(&self) -> Option<&str> {
        self.choices
            .selected
            .as_ref()
            .map(|choice| choice.preset.name())
    }

    pub(super) fn chosen_preset(&self) -> Option<&Preset> {
        self.choices.selected.as_ref().map(|choice| &choice.preset)
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn select_preset(name: String, cx: &mut App) {
        use gpui::BorrowAppContext as _;

        let (call, receipt) = Call::new(Command::Preset { name });
        cx.update_global::<Self, _>(|services, cx| services.control(call, cx));
        let sent = cx.global::<Self>().choices.sent;
        cx.spawn(async move |cx| {
            let Ok(reply) = receipt.recv().await else {
                return;
            };
            let Some(reason) = reply.reason else {
                return;
            };
            cx.update(|cx| {
                let services = cx.global::<Self>();
                if services.choices.sent == sent {
                    services.report(crate::runtime::Report::Failed(reason));
                }
            });
        })
        .detach();
    }

    pub(super) fn control(&mut self, call: Call, cx: &App) {
        if !call.is_live() {
            return;
        }
        if self.is_quitting() || self.mode.is_demo() {
            call.respond(Reply::refused("Commands require the running desktop app."));
            return;
        }
        match &call.command {
            Command::Style { name } => {
                self.select_style(*name);
                call.respond(Reply::accepted());
            },
            Command::Preset { name } => self.choose_preset(name.clone(), call, cx),
            Command::Status if self.live_runtime().is_none() => {
                call.respond(Reply::ok(self.control_status()));
            },
            _ => match self.runtime() {
                Some(runtime) => runtime.control(call),
                None => call.respond(Reply::refused(
                    "Resume dictation before using this command.",
                )),
            },
        }
    }

    fn choose_preset(&mut self, name: String, call: Call, cx: &App) {
        if self.runtime().is_none() || self.output.borrow().phase.is_active() {
            call.respond(Reply::refused(
                "Finish the current session before choosing a preset.",
            ));
            return;
        }
        let Some(preset) = self
            .config
            .presets
            .iter()
            .find(|preset| preset.name() == name)
            .cloned()
        else {
            call.respond(Reply::refused(
                "That saved preset is unavailable. Save it in Settings first.",
            ));
            return;
        };
        let baseline = self.config.clone();
        let mut candidate = baseline.clone();
        preset.load(&mut candidate);
        let path = self.path.clone();
        let session = self.output.borrow().session;
        let request = self.choices.request();
        let selection = Selection {
            request,
            session,
            preset,
            baseline,
        };
        let validation = cx
            .background_executor()
            .spawn(async move { candidate.validated(&path) });
        self.choices.choosing = Some(cx.spawn(async move |cx| {
            let result = validation.await;
            let admission = cx.update_global::<Self, _>(|services, _| {
                services.admit_preset(selection, result, &call)
            });
            let (selected, receipt) = match admission {
                Ok(admission) => admission,
                Err(reason) => {
                    cx.update_global::<Self, _>(|services, _| {
                        services.choices.answer(request, false)
                    });
                    call.respond(Reply::refused(reason));
                    return;
                },
            };
            let reply = receipt
                .recv()
                .await
                .unwrap_or_else(|_| Reply::unknown(UNACKNOWLEDGED));
            let reply = cx.update_global::<Self, _>(|services, _| {
                services.confirm_preset(request, selected, reply)
            });
            call.respond(reply);
        }));
    }

    fn admit_preset(
        &mut self,
        selection: Selection,
        result: anyhow::Result<ValidConfig>,
        caller: &Call,
    ) -> Result<(Selected, Receiver<Reply>), &'static str> {
        let Selection {
            request,
            session,
            preset,
            baseline,
        } = selection;
        if !caller.is_live() || self.choices.sent != request {
            return Err("Preset selection expired or was replaced. Choose it again.");
        }
        let config = result.map_err(
            |_invalid| "This preset's engine or model is unavailable. Open Settings to repair it.",
        )?;
        let runtime = self
            .runtime()
            .ok_or("Resume dictation before choosing a preset.")?;
        let (call, receipt) = caller.forwarded();
        runtime.preset(
            preset.name().to_owned(),
            config.clone(),
            baseline,
            session,
            call,
        );
        if let Some(acknowledging) = self.choices.choosing.take() {
            acknowledging.detach();
        }
        Ok((Selected { preset, config }, receipt))
    }

    fn confirm_preset(&mut self, request: u64, selected: Selected, reply: Reply) -> Reply {
        let accepted = reply.result == Disposition::Ok;
        if !self.choices.answer(request, accepted) {
            return if accepted {
                Reply::refused(REPLACED)
            } else {
                reply
            };
        }
        self.choices.selected = Some(selected);
        self.output
            .send_modify(|snapshot| snapshot.preset = self.preset_name().map(str::to_owned));
        reply
    }

    fn control_status(&self) -> Status {
        let snapshot = self.output.borrow();
        Status {
            version: env!("CARGO_PKG_VERSION").into(),
            service: self.service_state().name().into(),
            phase: snapshot.phase.name().into(),
            model: snapshot.model.name().into(),
            desktop_ready: snapshot.desktop_ready,
            session: None,
            style: self.style(),
            preset: self.preset_name().map(str::to_owned),
            last_dictation: snapshot.last_dictation,
        }
    }

    pub(super) fn use_preset(
        &mut self,
        candidate: Config,
        current: impl Fn(&App) -> bool + 'static,
        cx: &App,
    ) -> Result<Receiver<Used>, &'static str> {
        if self.output.borrow().phase.is_active() {
            return Err(USE_BUSY);
        }
        let path = self.path.clone();
        let (answer, used) = async_channel::bounded(1);
        let validation = cx
            .background_executor()
            .spawn(async move { candidate.validated(&path) });
        self.choices.using = Some(cx.spawn(async move |cx| {
            let result = validation.await;
            let admission = cx.update_global::<Self, _>(|services, cx| {
                let current = current(cx);
                services.admit_use(result, current)
            });
            let outcome = match admission {
                Ok(Admission::Ready(config)) => Ok(config),
                Ok(Admission::Waiting(config, request, receipt)) => {
                    let adopted = receipt.recv().await.unwrap_or(Err(USE_UNACKNOWLEDGED));
                    cx.update_global::<Self, _>(|services, _| {
                        services.confirm_use(request, &config, adopted)
                    })
                    .map(|()| config)
                },
                Err(reason) => Err(reason),
            };
            if answer.try_send(outcome).is_err() {
                cx.update_global::<Self, _>(|services, _| services.restore_owner());
            }
        }));
        Ok(used)
    }

    pub(super) fn follow_draft(&mut self, draft: &Config) {
        if let Some(admitted) = &mut self.admitted {
            admitted.clone_from(draft);
        }
    }

    pub(super) fn restore_owner(&mut self) {
        self.admitted = None;
        let saved = self.config.clone();
        if self
            .runtime_mut()
            .is_some_and(|runtime| runtime.revert(saved))
        {
            self.choices.supersede();
            self.choices.selected = None;
        }
    }

    fn admit_use(
        &mut self,
        result: anyhow::Result<ValidConfig>,
        current: bool,
    ) -> Result<Admission, &'static str> {
        if !current {
            return Err(USE_OVERTAKEN);
        }
        let config = result.map_err(|_unavailable| USE_UNAVAILABLE)?;
        let Some(runtime) = self.live_runtime() else {
            if self.config.change_to(&config) >= Change::Session {
                self.admitted = Some(config.clone().into_inner());
            }
            self.choices.forget();
            return Ok(Admission::Ready(config));
        };
        if self.choices.is_settled()
            && self.choices.selected.is_none()
            && runtime.baseline().change_to(&config) < Change::Session
        {
            return Ok(Admission::Ready(config));
        }
        let receipt = runtime.adopt(config.clone());
        Ok(Admission::Waiting(config, self.choices.request(), receipt))
    }

    fn confirm_use(&mut self, request: u64, config: &ValidConfig, adopted: Adopted) -> Adopted {
        let honored = self.choices.answer(request, adopted.is_ok());
        adopted?;
        if !honored {
            return Err(USE_OVERTAKEN);
        }
        self.choices.selected = None;
        self.output.send_modify(|snapshot| snapshot.preset = None);
        if let Some(runtime) = self.runtime_mut() {
            runtime.settle(config.clone());
        }
        Ok(())
    }

    pub(super) fn reset_choice_for(&mut self, config: &Config) {
        let baseline = self.runtime().map_or(&self.config, Runtime::baseline);
        if baseline.change_to(config) >= Change::Session {
            self.choices.forget();
        }
    }
}

#[cfg(test)]
mod tests;
