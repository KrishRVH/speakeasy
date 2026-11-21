use async_channel::Receiver;
use gpui::{App, Task};
use speakeasy_core::lifecycle::ConfigEpoch;

use super::Services;
use crate::{
    config::{Change, Config, ValidConfig, presets::Preset},
    control::{Call, Command, Disposition, Reply, Status},
    runtime::Adopted,
};

const UNACKNOWLEDGED: &str =
    "Preset selection stopped before acknowledging. Read status before choosing again.";

const USE_BUSY: &str = "Finish the current session or preset selection before using a preset.";

pub(super) const USE_OVERTAKEN: &str = "Speech settings changed before the preset was ready, so it was not used. Use it again to apply it.";

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

struct Selection {
    generation: u64,
    epoch: ConfigEpoch,
    session: u64,
    preset: Preset,
    baseline: Config,
}

#[derive(Default)]
pub(super) struct Choices {
    pub selected: Option<Selected>,
    generation: u64,
    choosing: Option<Task<()>>,
    using: Option<Task<()>>,
    applying: bool,
}

impl Choices {
    pub(super) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.choosing = None;
        self.using = None;
        self.applying = false;
    }

    fn supersede(&mut self) -> bool {
        let acknowledging = self.choosing.take_if(|_| self.applying);
        let unsettled = acknowledging.is_some();
        if let Some(acknowledging) = acknowledging {
            acknowledging.detach();
        }
        self.generation = self.generation.wrapping_add(1);
        self.choosing = None;
        self.applying = false;
        unsettled
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
    pub(super) const fn is_applying(&self) -> bool {
        self.applying
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
        let generation = cx.global::<Self>().choices.generation;
        cx.spawn(async move |cx| {
            let Ok(reply) = receipt.recv().await else {
                return;
            };
            let Some(reason) = reply.reason else {
                return;
            };
            cx.update(|cx| {
                let services = cx.global::<Self>();
                if services.choices.generation == generation {
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
        if self.choices.applying
            || self.runtime().is_none()
            || self.output.borrow().phase.is_active()
        {
            call.respond(Reply::refused(
                "Finish the current session or preset selection before choosing a preset.",
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
        let epoch = self.config_epoch();
        self.choices.cancel();
        let generation = self.choices.generation;
        let selection = Selection {
            generation,
            epoch,
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
                    call.respond(Reply::refused(reason));
                    return;
                },
            };
            let reply = receipt
                .recv()
                .await
                .unwrap_or_else(|_| Reply::unknown(UNACKNOWLEDGED));
            cx.update_global::<Self, _>(|services, _| {
                services.confirm_preset(generation, selected, &reply);
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
            generation,
            epoch,
            session,
            preset,
            baseline,
        } = selection;
        if !caller.is_live()
            || self.choices.generation != generation
            || self.config_epoch() != epoch
        {
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
        self.choices.applying = true;
        Ok((Selected { preset, config }, receipt))
    }

    fn confirm_preset(&mut self, generation: u64, selected: Selected, reply: &Reply) {
        if self.choices.generation != generation {
            return;
        }
        self.choices.applying = false;
        if reply.result == Disposition::Ok {
            self.choices.selected = Some(selected);
            self.output
                .send_modify(|snapshot| snapshot.preset = self.preset_name().map(str::to_owned));
        }
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
                services.admit_use(result, current, cx)
            });
            let outcome = match admission {
                Ok(Admission::Ready(config)) => Ok(config),
                Ok(Admission::Waiting(config, receipt)) => {
                    let adopted = receipt.recv().await.unwrap_or(Err(UNACKNOWLEDGED));
                    cx.update_global::<Self, _>(|services, _| {
                        services.confirm_use(&config, adopted)
                    })
                    .map(|()| config)
                },
                Err(reason) => Err(reason),
            };
            if answer.try_send(outcome).is_err() {
                cx.update_global::<Self, _>(|services, cx| services.restore_owner(cx));
            }
        }));
        Ok(used)
    }

    pub(super) fn follow_draft(&mut self, draft: &Config, cx: &App) {
        let Some(admitted) = &mut self.admitted else {
            return;
        };
        if admitted == draft {
            return;
        }
        admitted.clone_from(draft);
        self.recheck(cx);
    }

    pub(super) fn restore_owner(&mut self, cx: &App) {
        self.admitted = None;
        let saved = self.config.clone();
        let Some((seen, reverted)) = self
            .runtime()
            .and_then(|runtime| runtime.revert(saved.clone()))
        else {
            return;
        };
        let generation = self.choices.generation;
        cx.spawn(async move |cx| {
            if !reverted.recv().await.is_ok_and(|adopted| adopted.is_ok()) {
                return;
            }
            cx.update_global::<Self, _>(|services, _| {
                if services.choices.generation == generation
                    && let Some(runtime) = services.runtime()
                {
                    runtime.settle_reverted(&seen, saved);
                }
            });
        })
        .detach();
    }

    fn admit_use(
        &mut self,
        result: anyhow::Result<ValidConfig>,
        current: bool,
        cx: &App,
    ) -> Result<Admission, &'static str> {
        if !current {
            return Err(USE_OVERTAKEN);
        }
        let config = result.map_err(|_unavailable| USE_UNAVAILABLE)?;
        let unsettled = self.choices.supersede();
        let Some(runtime) = self.live_runtime() else {
            if self.config.change_to(&config) >= Change::Session {
                self.admitted = Some(config.clone().into_inner());
            }
            self.forget_preset(cx);
            return Ok(Admission::Ready(config));
        };
        if !unsettled
            && self.preset_name().is_none()
            && self.config.change_to(&config) < Change::Session
        {
            return Ok(Admission::Ready(config));
        }
        let receipt = runtime.adopt(config.clone());
        self.choices.applying = true;
        Ok(Admission::Waiting(config, receipt))
    }

    fn confirm_use(&mut self, config: &ValidConfig, adopted: Adopted) -> Adopted {
        self.choices.applying = false;
        adopted?;
        self.choices.selected = None;
        self.output.send_modify(|snapshot| snapshot.preset = None);
        if let Some(runtime) = self.runtime() {
            runtime.settle(config.clone());
        }
        Ok(())
    }

    pub(super) fn reset_choice_for(&mut self, config: &Config) {
        if self.config.change_to(config) >= Change::Session {
            self.choices.cancel();
            self.choices.selected = None;
        }
    }
}
