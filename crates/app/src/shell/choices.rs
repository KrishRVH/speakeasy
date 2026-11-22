use async_channel::Receiver;
use gpui::{App, Task};

use super::Services;
use crate::{
    config::{Change, Config, ValidConfig, presets::Preset},
    control::{Call, Command, Disposition, Reply, Status},
    runtime::{Adopted, Runtime},
};

const UNACKNOWLEDGED: &str =
    "Dictation stopped before the preset was applied. Choose it again once dictation runs.";

const USE_UNACKNOWLEDGED: &str =
    "Dictation stopped before the preset was applied. Use it again once dictation runs.";

const USE_BUSY: &str = "Finish the current session before using a preset.";

const USE_UNAVAILABLE: &str =
    "This preset's engine or model is unavailable. Choose them again, then update this preset.";

pub(super) type Used = Result<ValidConfig, &'static str>;

enum Admission {
    Ready(ValidConfig),
    Waiting(ValidConfig, Receiver<Adopted>),
}

pub(super) struct Selected {
    pub preset: Preset,
    pub config: ValidConfig,
}

#[derive(Default)]
pub(super) struct Choices {
    pub selected: Option<Selected>,
    started: u64,
    tasks: Vec<(u64, Task<()>)>,
}

impl Choices {
    pub(super) fn cancel(&mut self) {
        self.tasks.clear();
    }

    pub(super) fn forget(&mut self) {
        self.selected = None;
    }

    const fn next(&mut self) -> u64 {
        self.started = self.started.wrapping_add(1);
        self.started
    }

    fn hold(&mut self, key: u64, task: Task<()>) {
        self.tasks.push((key, task));
    }

    fn release(&mut self, key: u64) {
        self.tasks.retain(|(held, _)| *held != key);
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
        cx.spawn(async move |cx| {
            let Ok(reply) = receipt.recv().await else {
                return;
            };
            let Some(reason) = reply.reason else {
                return;
            };
            cx.update(|cx| {
                cx.global::<Self>()
                    .report(crate::runtime::Report::Failed(reason));
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
        let mut candidate = self.config.clone();
        preset.load(&mut candidate);
        let path = self.path.clone();
        let key = self.choices.next();
        let validation = cx
            .background_executor()
            .spawn(async move { candidate.validated(&path) });
        let choosing = cx.spawn(async move |cx| {
            let result = validation.await;
            let admission = cx.update_global::<Self, _>(|services, _| {
                services.admit_preset(preset, result, &call)
            });
            let reply = match admission {
                Ok((selected, receipt)) => {
                    let reply = receipt
                        .recv()
                        .await
                        .unwrap_or_else(|_| Reply::refused(UNACKNOWLEDGED));
                    cx.update_global::<Self, _>(|services, _| {
                        services.confirm_preset(selected, reply)
                    })
                },
                Err(reason) => Reply::refused(reason),
            };
            call.respond(reply);
            cx.update_global::<Self, _>(|services, _| services.choices.release(key));
        });
        self.choices.hold(key, choosing);
    }

    fn admit_preset(
        &self,
        preset: Preset,
        result: anyhow::Result<ValidConfig>,
        caller: &Call,
    ) -> Result<(Selected, Receiver<Reply>), &'static str> {
        if !caller.is_live() {
            return Err("Preset selection expired. Choose it again.");
        }
        let config = result.map_err(
            |_invalid| "This preset's engine or model is unavailable. Open Settings to repair it.",
        )?;
        let runtime = self
            .runtime()
            .ok_or("Resume dictation before choosing a preset.")?;
        let (call, receipt) = caller.forwarded();
        runtime.preset(preset.name().to_owned(), config.clone(), call);
        Ok((Selected { preset, config }, receipt))
    }

    fn confirm_preset(&mut self, selected: Selected, reply: Reply) -> Reply {
        if reply.result != Disposition::Ok {
            return reply;
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
        cx: &App,
    ) -> Result<Receiver<Used>, &'static str> {
        if self.output.borrow().phase.is_active() {
            return Err(USE_BUSY);
        }
        let key = self.choices.next();
        let path = self.path.clone();
        let (answer, used) = async_channel::bounded(1);
        let validation = cx
            .background_executor()
            .spawn(async move { candidate.validated(&path) });
        let using = cx.spawn(async move |cx| {
            let result = validation.await;
            let admission = cx.update_global::<Self, _>(|services, _| services.admit_use(result));
            let outcome = match admission {
                Ok(Admission::Ready(config)) => Ok(config),
                Ok(Admission::Waiting(config, receipt)) => {
                    let adopted = receipt.recv().await.unwrap_or(Err(USE_UNACKNOWLEDGED));
                    cx.update_global::<Self, _>(|services, _| {
                        services.confirm_use(&config, adopted)
                    })
                    .map(|()| config)
                },
                Err(reason) => Err(reason),
            };
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Settings outlives every Use it starts until Quit cancels its preset tasks"
            )]
            let _ = answer.try_send(outcome);
            cx.update_global::<Self, _>(|services, _| services.choices.release(key));
        });
        self.choices.hold(key, using);
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

    fn admit_use(
        &mut self,
        result: anyhow::Result<ValidConfig>,
    ) -> Result<Admission, &'static str> {
        let config = result.map_err(|_unavailable| USE_UNAVAILABLE)?;
        let Some(runtime) = self.live_runtime() else {
            if self.config.change_to(&config) >= Change::Session {
                self.admitted = Some(config.clone().into_inner());
            }
            self.choices.forget();
            return Ok(Admission::Ready(config));
        };
        if self.choices.selected.is_none()
            && runtime.baseline().change_to(&config) < Change::Session
        {
            return Ok(Admission::Ready(config));
        }
        let receipt = runtime.adopt(config.clone());
        Ok(Admission::Waiting(config, receipt))
    }

    fn confirm_use(&mut self, config: &ValidConfig, adopted: Adopted) -> Adopted {
        adopted?;
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
