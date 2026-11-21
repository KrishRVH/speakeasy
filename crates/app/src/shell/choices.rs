use async_channel::{Receiver, Sender};
use gpui::{App, Task};

use super::Services;
use crate::{
    config::{Change, Config, ValidConfig, presets::Preset},
    control::{Call, Command, Disposition, Reply, Status},
    runtime::{Adopted, Runtime},
};

const UNACKNOWLEDGED: &str =
    "Dictation stopped before the preset was applied. Choose it again once dictation runs.";

const REPLACED: &str =
    "Newer settings replaced the preset before it was confirmed. Choose it again.";

const USE_UNACKNOWLEDGED: &str =
    "Dictation stopped before the preset was applied. Use it again once dictation runs.";

const USE_BUSY: &str = "Finish the current session before using a preset.";

pub(super) const USE_OVERTAKEN: &str = "Speech settings changed before the preset was ready, so it was not used. Use it again to apply it.";

const USE_UNAVAILABLE: &str =
    "This preset's engine or model is unavailable. Choose them again, then update this preset.";

pub(super) type Used = Result<Applied, &'static str>;

pub(super) struct Applied {
    config: ValidConfig,
    loaded: Sender<()>,
}

impl Applied {
    pub(super) fn load(self) -> ValidConfig {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Services stops waiting for Settings only when Quit cancels its preset tasks"
        )]
        let _ = self.loaded.try_send(());
        self.config
    }
}

pub(super) enum Admission {
    Ready(ValidConfig),
    Waiting(ValidConfig, u64, Receiver<Adopted>),
}

pub(super) struct Selected {
    pub preset: Preset,
    pub config: ValidConfig,
}

struct Selection {
    choice: u64,
    session: u64,
    preset: Preset,
    baseline: Config,
}

#[derive(Default)]
pub(super) struct Choices {
    pub selected: Option<Selected>,
    chosen: u64,
    sent: u64,
    honored: u64,
    waiting: u64,
    tasks: Vec<(u64, Task<()>)>,
}

impl Choices {
    pub(super) fn cancel(&mut self) {
        self.tasks.clear();
    }

    pub(super) fn forget(&mut self) {
        self.supersede();
        self.selected = None;
    }

    pub(super) fn supersede(&mut self) {
        self.sent = self.sent.wrapping_add(1);
        self.honored = self.sent;
    }

    pub(super) fn choose(&mut self) -> u64 {
        self.outdate();
        self.chosen
    }

    pub(super) const fn outdate(&mut self) {
        self.chosen = self.chosen.wrapping_add(1);
    }

    const fn is_newest(&self, choice: u64) -> bool {
        self.chosen == choice
    }

    fn forward(&mut self) -> u64 {
        self.sent = self.sent.wrapping_add(1);
        self.waiting = self.waiting.saturating_add(1);
        self.sent
    }

    fn answer(&mut self, request: u64, accepted: bool) -> bool {
        self.waiting = self.waiting.saturating_sub(1);
        let honored = accepted && request > self.honored;
        if honored {
            self.honored = request;
        }
        honored
    }

    pub(super) const fn is_settled(&self) -> bool {
        self.waiting == 0
    }

    fn hold(&mut self, choice: u64, task: Task<()>) {
        self.tasks.push((choice, task));
    }

    fn release(&mut self, choice: u64) {
        self.tasks.retain(|(held, _)| *held != choice);
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
    pub(super) const fn is_idle(&self) -> bool {
        self.tasks.is_empty()
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
        let chosen = cx.global::<Self>().choices.chosen;
        cx.spawn(async move |cx| {
            let Ok(reply) = receipt.recv().await else {
                return;
            };
            let Some(reason) = reply.reason else {
                return;
            };
            cx.update(|cx| {
                let services = cx.global::<Self>();
                if services.choices.chosen == chosen {
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
        let choice = self.choices.choose();
        let selection = Selection {
            choice,
            session,
            preset,
            baseline,
        };
        let validation = cx
            .background_executor()
            .spawn(async move { candidate.validated(&path) });
        let choosing = cx.spawn(async move |cx| {
            let result = validation.await;
            let admission = cx.update_global::<Self, _>(|services, _| {
                services.admit_preset(selection, result, &call)
            });
            let reply = match admission {
                Ok((request, selected, receipt)) => {
                    let reply = receipt
                        .recv()
                        .await
                        .unwrap_or_else(|_| Reply::refused(UNACKNOWLEDGED));
                    cx.update_global::<Self, _>(|services, _| {
                        services.confirm_preset(request, selected, reply)
                    })
                },
                Err(reason) => Reply::refused(reason),
            };
            call.respond(reply);
            cx.update_global::<Self, _>(|services, _| services.choices.release(choice));
        });
        self.choices.hold(choice, choosing);
    }

    fn admit_preset(
        &mut self,
        selection: Selection,
        result: anyhow::Result<ValidConfig>,
        caller: &Call,
    ) -> Result<(u64, Selected, Receiver<Reply>), &'static str> {
        let Selection {
            choice,
            session,
            preset,
            baseline,
        } = selection;
        if !caller.is_live() || !self.choices.is_newest(choice) {
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
        Ok((self.choices.forward(), Selected { preset, config }, receipt))
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
        let choice = self.choices.choose();
        let path = self.path.clone();
        let (answer, used) = async_channel::bounded(1);
        let validation = cx
            .background_executor()
            .spawn(async move { candidate.validated(&path) });
        let using = cx.spawn(async move |cx| {
            let result = validation.await;
            let admission = cx.update_global::<Self, _>(|services, cx| {
                let current = current(cx);
                services.admit_use(choice, result, current)
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
            let loaded = match outcome {
                Ok(config) => {
                    let (loaded, acknowledged) = async_channel::bounded(1);
                    let sent = answer.try_send(Ok(Applied { config, loaded })).is_ok();
                    drop(answer);
                    sent && acknowledged.recv().await.is_ok()
                },
                Err(reason) => {
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "A refused Use left the owner to newer requests, so a caller that left needs nothing undone"
                    )]
                    let _ = answer.try_send(Err(reason));
                    true
                },
            };
            cx.update_global::<Self, _>(|services, cx| {
                if !loaded {
                    services.restore_owner(cx);
                }
                services.choices.release(choice);
            });
        });
        self.choices.hold(choice, using);
        Ok(used)
    }

    pub(super) fn follow_draft(&mut self, draft: &Config) {
        if let Some(admitted) = &mut self.admitted {
            admitted.clone_from(draft);
        }
    }

    pub(super) fn restore_owner(&mut self, cx: &App) {
        self.admitted = None;
        let saved = self.config.clone();
        if self
            .runtime_mut()
            .is_some_and(|runtime| runtime.revert(saved))
        {
            self.choices.forget();
        }
        self.restart_when_idle(cx);
    }

    pub(super) fn admit_use(
        &mut self,
        choice: u64,
        result: anyhow::Result<ValidConfig>,
        current: bool,
    ) -> Result<Admission, &'static str> {
        if !current || !self.choices.is_newest(choice) {
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
        Ok(Admission::Waiting(config, self.choices.forward(), receipt))
    }

    fn confirm_use(&mut self, request: u64, config: &ValidConfig, adopted: Adopted) -> Adopted {
        let honored = self.choices.answer(request, adopted.is_ok());
        adopted?;
        if !honored {
            return Err(USE_OVERTAKEN);
        }
        self.choices.selected = None;
        self.output.send_modify(|snapshot| snapshot.preset = None);
        self.admitted = Some(Config::clone(config));
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
