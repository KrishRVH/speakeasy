//! The Settings view. It edits a draft that reaches Services only through a durable save, and owns
//! the setup, file picker, microphone scan, and saves it starts.

mod render;

use std::{path::PathBuf, sync::Arc, time::Duration};

use async_channel::Receiver;
use gpui::{App, BorrowAppContext, Context, SharedString, Task, Timer, Window};
use speakeasy_dictation::{
    audio::{self, InputDevice},
    config::{Config, Engine},
    lifecycle::ConfigEpoch,
    runtime::Snapshot,
    save::{SaveProgress, SaveQueue, SaveRequest, SavedConfig},
    setup::{Installed, Setup},
    status::Indicator,
    theme::Theme,
};
use tokio::sync::watch;

use super::{Services, demo, request_quit, window::choose_file};
use crate::gpui_ext::{EntityUpdate, WindowUpdate};

const PROGRESS_THROTTLE: Duration = Duration::from_millis(100);

pub(super) struct Settings {
    config: Config,
    presentation: Presentation,
    notice: Option<SharedString>,
    microphones: Vec<InputDevice>,
    file_picker: Option<Task<()>>,
    microphone_scan: Option<Task<()>>,
    preview: Option<Task<()>>,
    setup: Option<(Setup, Task<()>)>,
    retiring_setups: Vec<(Setup, Task<()>)>,
    saving: Option<Task<()>>,
    saves: SaveQueue,
    _updates: Task<()>,
    /// Keeps the Windows minimize-to-tray hook alive while Settings is open.
    #[cfg(target_os = "windows")]
    pub(super) minimize_to_tray: Option<Task<()>>,
}

impl Settings {
    pub(super) fn new(
        config: Config,
        mut updates: watch::Receiver<Snapshot>,
        cx: &Context<Self>,
    ) -> Self {
        let presentation = Presentation::new(&updates.borrow_and_update(), cx.global::<Services>());
        let follow_updates = cx.spawn(async move |this, cx| {
            while updates.changed().await.is_ok() {
                let snapshot = updates.borrow_and_update().clone();
                if this
                    .update(cx, |view, cx| view.apply_snapshot(&snapshot, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut view = Self {
            config,
            presentation,
            notice: None,
            microphones: Vec::new(),
            file_picker: None,
            microphone_scan: None,
            preview: None,
            setup: None,
            retiring_setups: Vec::new(),
            saving: None,
            saves: SaveQueue::default(),
            _updates: follow_updates,
            #[cfg(target_os = "windows")]
            minimize_to_tray: None,
        };
        view.refresh_microphones(cx);
        view
    }

    fn apply_snapshot(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        let presentation = Presentation::new(snapshot, cx.global::<Services>());
        if self.presentation != presentation {
            self.presentation = presentation;
            cx.notify();
        }
    }

    fn act(&mut self, action: Action, window: &Window, cx: &mut Context<Self>) {
        if cx.global::<Services>().quitting() {
            return;
        }
        match action {
            Action::Setup => self.toggle_setup(cx),
            Action::Engine => self.switch_engine(),
            Action::Choose(file) => self.choose(file, window, cx),
            Action::Refresh => self.refresh_microphones(cx),
            Action::Microphone => self.cycle_microphone(),
            Action::Language => self.toggle_language(),
            Action::Toggle(setting) => self.toggle(setting, cx),
            Action::Theme => self.cycle_theme(cx),
            Action::Save if is_demo(cx) => {
                self.notice = Some("Preview only. Run without --demo to enable dictation.".into());
            },
            Action::Save => self.save(cx),
            Action::Pause => {
                cx.update_global::<Services, _>(|services, cx| services.stop(cx));
                self.notice = None;
            },
            Action::Preview => self.play(cx),
            Action::Quit => request_quit(cx),
        }
        cx.notify();
    }

    fn switch_engine(&mut self) {
        self.config.engine = match self.config.engine {
            Engine::Whisper => Engine::Parakeet,
            Engine::Parakeet => Engine::Whisper,
        };
        self.notice = Some("Choose the executable and model for this engine, then save.".into());
    }

    fn mark_unsaved(&mut self) {
        self.notice = Some("Unsaved changes".into());
    }

    fn cycle_microphone(&mut self) {
        self.config.microphone =
            next_microphone(&self.microphones, self.config.microphone.as_deref());
        self.mark_unsaved();
    }

    /// Parakeet detects the language itself, so only Whisper offers a choice.
    fn toggle_language(&mut self) {
        if self.config.engine == Engine::Parakeet {
            return;
        }
        self.config.language = if self.config.language == "en" {
            "auto"
        } else {
            "en"
        }
        .into();
        self.mark_unsaved();
    }

    fn toggle(&mut self, setting: Setting, cx: &mut App) {
        let flag = setting.flag(&mut self.config);
        *flag = !*flag;
        self.mark_unsaved();
        if is_demo(cx) && setting == Setting::ReduceMotion {
            show_motion(self.config.prefers_reduced_motion(), cx);
            self.notice = None;
        }
    }

    fn cycle_theme(&mut self, cx: &mut App) {
        self.config.theme = self.config.theme.next();
        self.mark_unsaved();
        if is_demo(cx) {
            let theme = self.config.theme;
            cx.update_global::<Services, _>(|services, _| services.config.theme = theme);
            show_theme(theme, cx);
            self.notice = None;
        }
    }

    fn refresh_microphones(&mut self, cx: &Context<Self>) {
        // The demo is simulated and never touches audio devices.
        if is_demo(cx) {
            return;
        }
        let scan = audio::microphones();
        self.microphone_scan = Some(cx.spawn(async move |this, cx| {
            let result = scan.await;
            this.update_if_alive(cx, |view, cx| {
                match result {
                    Ok(microphones) => view.microphones = microphones,
                    Err(_) => {
                        view.notice =
                            Some("Cannot list microphones. Check OS audio settings.".into());
                    },
                }
                view.microphone_scan = None;
                cx.notify();
            });
        }));
    }

    fn choose(&mut self, file: EngineFile, window: &Window, cx: &Context<Self>) {
        if self.file_picker.is_some() {
            return;
        }
        let title = match (self.config.engine, file) {
            (Engine::Parakeet, EngineFile::Executable) => "Choose nemo-speech",
            (Engine::Parakeet, EngineFile::Model) => "Choose a Parakeet v3 GGUF model",
            (Engine::Whisper, EngineFile::Executable) => "Choose whisper-server",
            (Engine::Whisper, EngineFile::Model) => "Choose a Whisper GGML model",
        };
        let filter = match (self.config.engine, file) {
            (_, EngineFile::Executable) => ["Programs", "*.exe"],
            (Engine::Parakeet, EngineFile::Model) => ["Parakeet models", "*.gguf"],
            (Engine::Whisper, EngineFile::Model) => ["Whisper models", "*.bin"],
        };
        let picker = choose_file(window, title, filter, cx);
        self.file_picker = Some(cx.spawn(async move |this, cx| {
            let result = picker.await;
            this.update_if_alive(cx, |view, cx| view.finish_choice(result, file, cx));
        }));
    }

    fn finish_choice(
        &mut self,
        result: anyhow::Result<Option<PathBuf>>,
        file: EngineFile,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(path)) => {
                match file {
                    EngineFile::Executable => self.config.engine_executable = path,
                    EngineFile::Model => self.config.model = path,
                }
                self.mark_unsaved();
            },
            Ok(None) => {},
            Err(_) => self.notice = Some("Could not open the file picker. Try again.".into()),
        }
        self.file_picker = None;
        cx.notify();
    }

    pub(super) fn play(&mut self, cx: &Context<Self>) {
        let output = cx.global::<Services>().output.clone();
        self.preview = Some(cx.spawn(async move |_, _| demo::play(output).await));
    }

    fn toggle_setup(&mut self, cx: &Context<Self>) {
        match self.setup.take() {
            Some(setup) => self.pause_setup(setup, cx),
            None => self.start_setup(cx),
        }
    }

    pub(super) fn start_setup(&mut self, cx: &Context<Self>) {
        if cx.global::<Services>().quitting() {
            return;
        }
        let setup = match Setup::start() {
            Ok(setup) => setup,
            Err(error) => {
                self.notice = Some(error.to_string().into());
                return;
            },
        };
        let mut progress = setup.progress_changes();
        let outcome = setup.outcome();
        self.notice = None;
        let follow_setup = cx.spawn(async move |this, cx| {
            while progress.changed().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
                Timer::after(PROGRESS_THROTTLE).await;
            }
            let outcome = outcome.await;
            this.update_if_alive(cx, |view, cx| view.finish_setup(outcome, cx));
        });
        self.setup = Some((setup, follow_setup));
    }

    fn pause_setup(&mut self, (setup, follow_setup): (Setup, Task<()>), cx: &Context<Self>) {
        drop(follow_setup);
        setup.request_stop();
        let stopped = setup.stopped();
        let retirement = cx.spawn(async move |this, cx| {
            stopped.await;
            this.update_if_alive(cx, |view, _| {
                view.retiring_setups
                    .retain(|(setup, _)| !setup.is_finished());
            });
        });
        self.retiring_setups.push((setup, retirement));
        self.notice = Some("Setup paused. Downloads resume where they stopped.".into());
    }

    fn finish_setup(&mut self, outcome: Option<anyhow::Result<Installed>>, cx: &mut Context<Self>) {
        self.setup = None;
        match outcome {
            Some(Ok(installed)) => {
                installed.apply_to(&mut self.config);
                self.save(cx);
            },
            Some(Err(error)) => self.notice = Some(error.to_string().into()),
            None => self.notice = Some("Setup stopped unexpectedly. Try again.".into()),
        }
        cx.notify();
    }

    fn request_setup_stops(&self) {
        for (setup, _) in self.setup.iter().chain(&self.retiring_setups) {
            setup.request_stop();
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let services = cx.global::<Services>();
        if services.quitting() {
            return;
        }
        let request = SaveRequest {
            draft: self.config.clone(),
            epoch: services.config_epoch,
            path: services.path.clone(),
        };
        self.notice = Some("Saving changes…".into());
        if let Some(progress) = self.saves.submit(request) {
            self.advance_save(progress, cx);
        }
        cx.notify();
    }

    fn advance_save(&mut self, progress: SaveProgress, cx: &mut Context<Self>) {
        match progress {
            SaveProgress::Writing(result) => self.saving = Some(Self::follow_write(result, cx)),
            SaveProgress::Finished {
                saved,
                error,
                epoch,
            } => {
                self.saving = None;
                self.finish_save(saved, error, epoch, cx);
            },
        }
    }

    fn follow_write(result: Receiver<anyhow::Result<SavedConfig>>, cx: &Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let completed = result.recv().await.unwrap_or_else(|_| {
                Err(anyhow::anyhow!(
                    "Saving settings stopped unexpectedly. Try saving again."
                ))
            });
            this.update_if_alive(cx, |view, cx| {
                let progress = view.saves.finish(completed);
                view.advance_save(progress, cx);
                cx.notify();
            });
        })
    }

    fn finish_save(
        &mut self,
        saved: Option<Box<SavedConfig>>,
        error: Option<String>,
        epoch: ConfigEpoch,
        cx: &mut Context<Self>,
    ) {
        let Some(saved) = saved else {
            self.notice = error.map(SharedString::from);
            return;
        };
        let unchanged = saved.update_draft(&mut self.config);
        let applied = cx.update_global::<Services, _>(|services, cx| {
            services.adopt_saved(saved.validated, epoch, cx)
        });
        self.notice = error
            .or_else(|| applied.err().map(|error| error.to_string()))
            .map(SharedString::from);
        if self.notice.is_none() && !unchanged {
            self.mark_unsaved();
        }
        show_motion(cx.global::<Services>().config.prefers_reduced_motion(), cx);
        show_theme(cx.global::<Services>().config.theme, cx);
    }

    pub(super) fn cancel_background_work(&mut self) {
        self.file_picker = None;
        self.microphone_scan = None;
        self.preview = None;
        self.request_setup_stops();
    }

    /// Releases setups whose threads have exited; returns whether no setup or save remains.
    pub(super) fn drain_finished(&mut self) -> bool {
        self.setup = self.setup.take().filter(|(setup, _)| !setup.is_finished());
        self.retiring_setups
            .retain(|(setup, _)| !setup.is_finished());
        self.setup.is_none() && self.retiring_setups.is_empty() && self.saves.is_idle()
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        self.request_setup_stops();
        // Quit may release this view before its save task sees a completion, so finish the writes
        // here, in order, before joining setups.
        self.saves.flush();
        self.setup = None;
        self.retiring_setups.clear();
    }
}

/// What Settings shows from the latest snapshot; a new snapshot redraws the view only when this
/// changes.
#[derive(PartialEq, Eq)]
struct Presentation {
    indicator: Indicator,
    shortcut: Arc<str>,
    cancel_shortcut: Arc<str>,
    message: String,
}

impl Presentation {
    fn new(snapshot: &Snapshot, services: &Services) -> Self {
        let status = services.status(snapshot);
        Self {
            indicator: status.indicator,
            shortcut: snapshot.shortcut.clone(),
            cancel_shortcut: snapshot.cancel_shortcut.clone(),
            message: if snapshot.message.is_empty() {
                status.description.into_owned()
            } else {
                snapshot.message.clone()
            },
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    Setup,
    Engine,
    Choose(EngineFile),
    Microphone,
    Refresh,
    Language,
    Toggle(Setting),
    Theme,
    Save,
    Pause,
    Preview,
    Quit,
}

impl Action {
    /// A stable element id, so a button keeps keyboard focus when its label changes.
    const fn element_id(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Engine => "engine",
            Self::Choose(EngineFile::Executable) => "choose-executable",
            Self::Choose(EngineFile::Model) => "choose-model",
            Self::Microphone => "microphone",
            Self::Refresh => "refresh",
            Self::Language => "language",
            Self::Toggle(Setting::PreferGpu) => "prefer-gpu",
            Self::Toggle(Setting::KeepClipboard) => "keep-clipboard",
            Self::Toggle(Setting::ReduceMotion) => "reduce-motion",
            Self::Toggle(Setting::RemoveFillers) => "remove-fillers",
            Self::Toggle(Setting::ManualPaste) => "manual-paste",
            Self::Toggle(Setting::TerminalPaste) => "terminal-paste",
            Self::Toggle(Setting::DesktopBindings) => "desktop-bindings",
            Self::Theme => "theme",
            Self::Save => "save",
            Self::Pause => "pause",
            Self::Preview => "preview",
            Self::Quit => "quit",
        }
    }
}

#[derive(Clone, Copy)]
enum EngineFile {
    Executable,
    Model,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Setting {
    PreferGpu,
    KeepClipboard,
    ReduceMotion,
    RemoveFillers,
    ManualPaste,
    TerminalPaste,
    DesktopBindings,
}

impl Setting {
    fn flag(self, config: &mut Config) -> &mut bool {
        match self {
            Self::PreferGpu => &mut config.use_gpu,
            Self::KeepClipboard => &mut config.preserve_clipboard,
            Self::ReduceMotion => &mut config.reduced_motion,
            Self::RemoveFillers => &mut config.remove_fillers,
            Self::ManualPaste => &mut config.linux.manual_paste,
            Self::TerminalPaste => &mut config.linux.terminal_paste,
            Self::DesktopBindings => &mut config.linux.external_shortcut,
        }
    }

    fn is_on(self, config: &Config) -> bool {
        match self {
            Self::PreferGpu => config.use_gpu,
            Self::KeepClipboard => config.preserve_clipboard,
            Self::ReduceMotion => config.reduced_motion,
            Self::RemoveFillers => config.remove_fillers,
            Self::ManualPaste => config.linux.manual_paste,
            Self::TerminalPaste => config.linux.terminal_paste,
            Self::DesktopBindings => config.linux.external_shortcut,
        }
    }
}

fn is_demo(cx: &App) -> bool {
    cx.global::<Services>().mode.is_demo()
}

/// Cycles through the listed microphones, then back to the system default (`None`).
fn next_microphone(microphones: &[InputDevice], current: Option<&str>) -> Option<String> {
    let next = match current {
        None => microphones.first(),
        Some(current) => microphones
            .iter()
            .skip_while(|device| device.id != current)
            .nth(1),
    };
    next.map(|device| device.id.clone())
}

/// The pill and tray show the saved theme; only Settings previews the draft.
fn show_theme(theme: Theme, cx: &mut App) {
    let pill = cx.global::<Services>().pill;
    pill.update_if_open(cx, |view, _, cx| {
        view.set_theme(theme);
        cx.notify();
    });
    // Republishing the current snapshot makes the tray redraw its icon.
    cx.global::<Services>().output.send_modify(|_| ());
}

fn show_motion(reduced_motion: bool, cx: &mut App) {
    let pill = cx.global::<Services>().pill;
    pill.update_if_open(cx, |view, _, cx| {
        view.set_reduced_motion(reduced_motion);
        cx.notify();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings that pass validation: a fake engine and model written into `directory`.
    fn valid_config(directory: &std::path::Path) -> anyhow::Result<Config> {
        use std::os::unix::fs::PermissionsExt;
        let engine_executable = directory.join("engine");
        let model = directory.join("model.gguf");
        std::fs::write(&engine_executable, b"fake engine")?;
        std::fs::write(&model, b"fake model")?;
        std::fs::set_permissions(&engine_executable, std::fs::Permissions::from_mode(0o700))?;
        Ok(Config {
            engine_executable,
            model,
            ..Config::default()
        })
    }

    fn microphones(ids: &[&str]) -> Vec<InputDevice> {
        ids.iter()
            .map(|id| InputDevice {
                id: (*id).to_owned(),
                name: String::new(),
            })
            .collect()
    }

    #[test]
    fn microphone_choice_cycles_through_devices_then_back_to_the_default() {
        let listed = microphones(&["first", "second"]);
        assert_eq!(next_microphone(&listed, None).as_deref(), Some("first"));
        assert_eq!(
            next_microphone(&listed, Some("first")).as_deref(),
            Some("second")
        );
        assert_eq!(next_microphone(&listed, Some("second")), None);
        assert_eq!(next_microphone(&listed, Some("unplugged")), None);
    }

    #[test]
    fn dropping_settings_writes_the_latest_queued_save() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let config = valid_config(directory.path())?;
        let mut saves = SaveQueue::default();
        let mut submit = |threads| {
            saves.submit(SaveRequest {
                draft: Config {
                    threads,
                    ..config.clone()
                },
                epoch: ConfigEpoch::default(),
                path: path.clone(),
            })
        };
        assert!(submit(4).is_some(), "The first save did not start");
        assert!(submit(8).is_none(), "The latest save did not queue");
        drop(Settings {
            config,
            presentation: Presentation {
                indicator: Indicator::Paused,
                shortcut: Arc::from(""),
                cancel_shortcut: Arc::from(""),
                message: String::new(),
            },
            notice: None,
            microphones: Vec::new(),
            file_picker: None,
            microphone_scan: None,
            preview: None,
            setup: None,
            retiring_setups: Vec::new(),
            saving: None,
            saves,
            _updates: Task::ready(()),
            #[cfg(target_os = "windows")]
            minimize_to_tray: None,
        });
        assert_eq!(Config::read(&path)?.threads, 8);
        Ok(())
    }
}
