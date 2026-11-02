use gpui::{App, BorrowAppContext as _, Task};

use super::{Services, lifecycle::ConfigEpoch};
use crate::{
    config::{Change, Config, ValidConfig},
    control::{Call, Command, Disposition, Reply, Status},
    gpui_ext::AppUpdate as _,
    status::ServiceState,
};

pub(super) struct Selected {
    pub name: String,
    pub config: ValidConfig,
}

struct Selection {
    generation: u64,
    epoch: ConfigEpoch,
    session: u64,
    name: String,
    baseline: Config,
}

#[derive(Default)]
pub(super) struct Choices {
    pub selected: Option<Selected>,
    generation: u64,
    task: Option<Task<()>>,
    applying: bool,
}

impl Choices {
    pub(super) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.task = None;
        self.applying = false;
    }
}

impl Services {
    pub(crate) fn preset_name(&self) -> Option<&str> {
        self.choices
            .selected
            .as_ref()
            .map(|choice| choice.name.as_str())
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn select_preset(name: String, cx: &mut App) {
        let (call, receipt) = Call::new(Command::Preset { name });
        cx.update_global::<Self, _>(|services, cx| services.control(call, cx));
        let generation = cx.global::<Services>().choices.generation;
        cx.spawn(async move |cx| {
            let Ok(reply) = receipt.recv().await else {
                return;
            };
            let Some(reason) = reply.reason else {
                return;
            };
            cx.update_if_running(|cx| {
                let services = cx.global::<Services>();
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
            Command::Status if self.runtime().is_none() => {
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
            name,
            baseline,
        };
        let validation = cx
            .background_executor()
            .spawn(async move { candidate.validated(&path) });
        self.choices.task = Some(cx.spawn(async move |cx| {
            let result = validation.await;
            let admission = cx.update(|cx| {
                cx.update_global::<Self, _>(|services, _| {
                    services.admit_preset(selection, result, &call)
                })
            });
            let (selected, receipt) = match admission {
                Ok(Ok(admission)) => admission,
                Ok(Err(reason)) => {
                    call.respond(Reply::refused(reason));
                    return;
                },
                Err(_) => return,
            };
            let reply = receipt.recv().await.unwrap_or_else(|_| Reply::unknown("Preset selection stopped before acknowledging. Read status before choosing again."));
            cx.update_if_running(|cx| {
                cx.update_global::<Self, _>(|services, _| {
                    services.confirm_preset(generation, selected, &reply);
                });
            });
            call.respond(reply);
        }));
    }

    fn admit_preset(
        &mut self,
        selection: Selection,
        result: anyhow::Result<ValidConfig>,
        caller: &Call,
    ) -> Result<(Selected, async_channel::Receiver<Reply>), &'static str> {
        let Selection {
            generation,
            epoch,
            session,
            name,
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
        runtime.preset(name.clone(), config.clone(), baseline, session, call);
        self.choices.applying = true;
        Ok((Selected { name, config }, receipt))
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

    pub(super) fn control_status(&self) -> Status {
        let snapshot = self.output.borrow();
        Status {
            version: env!("CARGO_PKG_VERSION").into(),
            service: match self.service_state() {
                ServiceState::Running => "running",
                ServiceState::Paused => "paused",
                ServiceState::Pausing => "pausing",
                ServiceState::Quitting => "quitting",
                ServiceState::NotSetUp => "not_set_up",
            }
            .into(),
            phase: snapshot.phase.name().into(),
            model: snapshot.model.name().into(),
            desktop_ready: snapshot.desktop_ready,
            session: None,
            style: self.style(),
            preset: self.preset_name().map(str::to_owned),
            last_dictation: snapshot.last_dictation,
        }
    }

    pub(super) fn reset_choice_for(&mut self, config: &Config) {
        if self.config.change_to(config) >= Change::Session {
            self.choices.cancel();
            self.choices.selected = None;
        }
    }
}
