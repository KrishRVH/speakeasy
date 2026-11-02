mod language;
mod navigation;
pub(super) mod onboarding;
mod permissions;
mod practice;
mod presets;
mod recognition;
mod render;
mod speech;
mod text_field;
mod updates;
mod vocabulary;

use std::{path::PathBuf, sync::Arc, time::Duration};

use async_channel::Receiver;
use gpui::{App, BorrowAppContext, Context, SharedString, Task, Timer, Window};
use speakeasy_platform::{FileFilter, Permissions};
use tokio::sync::watch;
use updates::Update;

use super::{
    Services, demo,
    lifecycle::ConfigEpoch,
    request_quit,
    save::{SaveProgress, SaveQueue, SavedConfig},
    window::choose_file,
};
use crate::{
    audio::{InputDevice, MicrophoneScan},
    config::{Config, Engine},
    gpui_ext::{EntityUpdate, WindowUpdate, follow},
    models::Model,
    runtime::Snapshot,
    setup::{Installed, Setup},
    status::{Indicator, Readiness},
};

const PROGRESS_THROTTLE: Duration = Duration::from_millis(100);

pub(super) struct Settings {
    config: Config,
    presentation: Presentation,
    permissions: Option<Permissions>,
    notice: Option<Notice>,
    microphones: Vec<InputDevice>,
    file_picker: Option<Task<()>>,
    microphone_listing: Option<Task<()>>,
    preview: Option<Task<()>>,
    setup: Option<(Setup, Task<()>, Config)>,
    retiring_setup: Option<(Setup, Task<()>)>,
    saving: Option<Task<()>>,
    saves: SaveQueue,
    vocabulary: vocabulary::Editor,
    recognition: recognition::Editor,
    presets: presets::Editor,
    language: language::Picker,
    navigation: navigation::Navigation,
    practice: practice::Practice,
    onboarding: Option<onboarding::Step>,
    onboarding_focus: gpui::FocusHandle,
    onboarding_completion: Option<onboarding::Completion>,
    update: Update,
    _updates: Task<()>,
    unreadable: bool,
    reset: Reset,
    downloaded: Vec<Model>,
    own_engine: bool,
    startup_apps_off: bool,
    #[cfg(target_os = "linux")]
    desktop_command: SharedString,
    #[cfg(target_os = "windows")]
    pub(super) minimize_to_tray: Option<Task<()>>,
}

impl Settings {
    pub(super) fn new(
        config: Config,
        mut updates: watch::Receiver<Snapshot>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let services = cx.global::<Services>();
        let presentation = Presentation::new(&updates.borrow_and_update(), services);
        let saves = SaveQueue::new(services.path.clone(), config.clone());
        #[cfg(target_os = "linux")]
        let desktop_command = desktop_command(&services.path, services.default_settings).into();
        let onboarding = (!services.mode.is_demo() && onboarding::pending(&services.path))
            .then_some(onboarding::Step::Prepare);
        let unreadable = !services.mode.is_demo() && Config::is_unreadable(&services.path);
        let vocabulary = vocabulary::Editor::new(config.theme, cx);
        let recognition = recognition::Editor::new(config.theme, cx);
        let presets = presets::Editor::new(config.theme, cx);
        let mut view = Self {
            config,
            presentation,
            permissions: None,
            notice: None,
            microphones: Vec::new(),
            file_picker: None,
            microphone_listing: None,
            preview: None,
            setup: None,
            retiring_setup: None,
            saving: None,
            saves,
            vocabulary,
            recognition,
            presets,
            language: language::Picker::new(cx),
            navigation: navigation::Navigation::new(cx),
            practice: practice::Practice::default(),
            onboarding,
            onboarding_focus: cx.focus_handle(),
            onboarding_completion: None,
            update: Update::Idle,
            _updates: follow(updates, cx, Snapshot::clone, |view, snapshot, cx| {
                view.apply_snapshot(&snapshot, cx);
            }),
            unreadable,
            reset: Reset::Idle,
            downloaded: crate::setup::downloaded(),
            own_engine: false,
            startup_apps_off: false,
            #[cfg(target_os = "linux")]
            desktop_command,
            #[cfg(target_os = "windows")]
            minimize_to_tray: None,
        };
        view.refresh_microphones(cx);
        view
    }

    #[cfg(target_os = "windows")]
    pub(super) fn follow_startup_apps(&mut self, window: &mut Window, cx: &Context<'_, Self>) {
        self.startup_apps_off = !speakeasy_platform::startup_approved();
        cx.observe_window_activation(window, |view, window, cx| {
            if window.is_window_active() {
                view.startup_apps_off = !speakeasy_platform::startup_approved();
                cx.notify();
            }
        })
        .detach();
    }

    fn apply_snapshot(&mut self, snapshot: &Snapshot, cx: &mut Context<'_, Self>) {
        let presentation = Presentation::new(snapshot, cx.global::<Services>());
        if self.presentation != presentation {
            self.presentation = presentation;
            cx.notify();
        }
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<'_, Self>) {
        if cx.global::<Services>().is_quitting() {
            return;
        }
        if matches!(
            action,
            Action::Pause
                | Action::Resume
                | Action::Engine(_)
                | Action::OwnEngine
                | Action::Setup
                | Action::ConfirmReset
        ) {
            self.practice.stop();
        }
        match action {
            Action::Onboarding(action) => self.onboarding_action(action, window, cx),
            Action::Page(page) => self.open_page(page, window),
            Action::Setup => self.toggle_setup(cx),
            Action::Install(model) => self.install_model(model, cx),
            Action::OwnEngine => self.use_own_engine(),
            Action::Engine(engine) => self.switch_engine(engine),
            Action::Reset => {
                if self.can_reset(cx) {
                    self.reset = Reset::Confirming;
                }
            },
            Action::ConfirmReset => self.confirm_reset(cx),
            Action::CancelReset => {
                if matches!(self.reset, Reset::Confirming) {
                    self.reset = Reset::Idle;
                }
            },
            Action::Choose(file) => self.choose(file, window, cx),
            Action::Refresh => self.refresh_microphones(cx),
            Action::Microphone => self.cycle_microphone(),
            Action::Language => self.toggle_language(window, cx),
            Action::SelectLanguage(language) => {
                self.config.language = if self.config.engine == Engine::Whisper {
                    crate::models::whisper_language(language)
                } else {
                    language
                }
                .into();
                self.close_language(window);
                self.edited();
            },
            Action::Delivery => self.cycle_delivery(),
            #[cfg(target_os = "macos")]
            Action::MacShortcut => {
                self.config.mac_shortcut = self.config.mac_shortcut.next();
                self.edited();
            },
            Action::Toggle(setting) => self.toggle(setting, cx),
            Action::Theme => self.cycle_theme(cx),
            Action::Save => self.save(cx),
            Action::Revert => {
                if self.setup.is_none() && self.saving.is_none() {
                    self.config = cx.global::<Services>().config.clone();
                    self.vocabulary.clear(cx);
                    self.recognition.clear(cx);
                    self.presets.clear(cx);
                    self.language.cursor = None;
                    self.own_engine = false;
                    self.edited();
                }
            },
            Action::Resume => {
                cx.update_global::<Services, _>(|services, cx| services.resume(cx));
                self.notice = None;
            },
            Action::Pause => {
                cx.update_global::<Services, _>(|services, cx| services.pause(cx));
                self.notice = None;
            },
            Action::Preview => self.play(cx),
            Action::Practice => self.toggle_practice(cx),
            Action::CheckUpdates => self.check_updates(cx),
            Action::OpenUpdate => {
                if let Some(version) = self.update.version() {
                    cx.open_url(&version.release_url());
                }
            },
            #[cfg(target_os = "windows")]
            Action::DownloadUpdate => self.download_update(cx),
            #[cfg(target_os = "windows")]
            Action::CancelUpdate => match &self.update {
                Update::Downloading { .. } => {
                    self.update.cancel();
                    self.notice = Some(Notice::Note("Cancelling update download…".into()));
                },
                Update::Ready(installer) => {
                    self.update = Update::Available(installer.version());
                    self.notice = Some(Notice::Note("Update download cancelled.".into()));
                },
                _ => {},
            },
            #[cfg(target_os = "windows")]
            Action::InstallUpdate => self.install_update(cx),
            Action::PermissionHelp(help) => cx.open_url(help.url()),
            Action::Shows => super::reveal_shows(cx),
            Action::Quit => request_quit(cx),
        }
        cx.notify();
    }

    fn use_own_engine(&mut self) {
        speech::use_own_engine(&mut self.config);
        self.own_engine = true;
        self.edited();
    }

    fn switch_engine(&mut self, engine: Engine) {
        if speech::switch_engine(&mut self.config, engine) {
            self.language.cursor = None;
            self.edited();
        }
    }

    const fn resetting(&self) -> bool {
        matches!(self.reset, Reset::Running { .. })
    }

    fn can_reset(&self, cx: &App) -> bool {
        let services = cx.global::<Services>();
        !services.mode.is_demo()
            && !services.is_quitting()
            && self.setup.is_none()
            && self.retiring_setup.is_none()
            && self.saving.is_none()
            && !self.resetting()
            && !self.update.holds_download()
    }

    fn confirm_reset(&mut self, cx: &mut Context<'_, Self>) {
        if !matches!(self.reset, Reset::Confirming) || !self.can_reset(cx) {
            return;
        }
        let receipt = match cx.update_global::<Services, _>(|services, cx| services.reset(cx)) {
            Ok(receipt) => receipt,
            Err(error) => {
                self.reset = Reset::Idle;
                self.notice = Some(Notice::Problem(format!("{error:#}").into()));
                return;
            },
        };
        self.notice = None;
        self.reset = Reset::Running {
            _work: cx.spawn(async move |this, cx| {
                let result = receipt.recv().await.unwrap_or_else(|_closed| {
                    Err("The reset stopped before removing anything. Try again.".into())
                });
                this.update_if_alive(cx, |view, cx| view.finish_reset(result, cx));
            }),
        };
    }

    fn finish_reset(&mut self, result: Result<(), String>, cx: &mut Context<'_, Self>) {
        self.reset = Reset::Idle;
        let path = cx.global::<Services>().path.clone();
        self.unreadable = Config::is_unreadable(&path);
        self.downloaded = crate::setup::downloaded();
        match result {
            Ok(()) => {
                self.config = Config::default();
                self.saves = SaveQueue::new(path, Config::default());
                self.vocabulary.clear(cx);
                self.recognition.clear(cx);
                self.presets.clear(cx);
                self.language.cursor = None;
                self.own_engine = false;
                self.file_picker = None;
                show_motion(self.config.prefers_reduced_motion(), cx);
                show_theme(cx);
                self.start_setup(cx);
            },
            Err(error) => self.notice = Some(Notice::Problem(error.into())),
        }
        cx.notify();
    }

    fn edited(&mut self) {
        self.practice.stop();
        self.notice = None;
    }

    fn unsaved(&self, cx: &App) -> bool {
        !is_demo(cx)
            && (self.saves.needs_save(&self.config)
                || self.vocabulary.pending(cx)
                || self.recognition.pending(cx)
                || self.presets.pending(cx))
    }

    fn cycle_microphone(&mut self) {
        self.config.microphone =
            next_microphone(&self.microphones, self.config.microphone.as_deref());
        self.edited();
    }

    fn cycle_delivery(&mut self) {
        self.config.text_delivery = self.config.text_delivery.next();
        self.edited();
    }

    fn toggle(&mut self, setting: Setting, cx: &mut App) {
        let flag = setting.flag(&mut self.config);
        *flag = !*flag;
        self.edited();
        if is_demo(cx) && setting == Setting::ReduceMotion {
            show_motion(self.config.prefers_reduced_motion(), cx);
        }
    }

    fn cycle_theme(&mut self, cx: &mut App) {
        self.config.theme = self.config.theme.next();
        self.edited();
        if is_demo(cx) {
            cx.global_mut::<Services>().config.theme = self.config.theme;
            show_theme(cx);
        }
    }

    fn refresh_microphones(&mut self, cx: &mut Context<'_, Self>) {
        let services = cx.global::<Services>();
        if services.mode.is_demo() || services.is_quitting() || self.microphone_listing.is_some() {
            return;
        }
        let listing = MicrophoneScan::listing(&mut cx.global_mut::<Services>().microphone_scan);
        self.microphone_listing = Some(cx.spawn(async move |this, cx| {
            let result = async move { listing?.await }.await;
            this.update_if_alive(cx, |view, cx| {
                match result {
                    Ok(microphones) => view.microphones = microphones,
                    Err(_) => {
                        view.notice = Some(Notice::Problem(
                            "Cannot list microphones. Check your system's sound settings.".into(),
                        ));
                    },
                }
                view.microphone_listing = None;
                cx.notify();
            });
        }));
    }

    fn choose(&mut self, file: EngineFile, window: &Window, cx: &Context<'_, Self>) {
        if self.file_picker.is_some() {
            return;
        }
        let title = match (self.config.engine, file) {
            (Engine::Nemo, EngineFile::Executable) => "Choose nemo-speech",
            (Engine::Nemo, EngineFile::Model) => "Choose a NeMo GGUF model",
            (Engine::Whisper, EngineFile::Executable) => "Choose whisper-server",
            (Engine::Whisper, EngineFile::Model) => "Choose a Whisper GGML model",
        };
        let filter = match (self.config.engine, file) {
            (_, EngineFile::Executable) => FileFilter {
                name: "Programs",
                pattern: "*.exe",
            },
            (Engine::Nemo, EngineFile::Model) => FileFilter {
                name: "NeMo models",
                pattern: "*.gguf",
            },
            (Engine::Whisper, EngineFile::Model) => FileFilter {
                name: "Whisper models",
                pattern: "*.bin",
            },
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
        cx: &mut Context<'_, Self>,
    ) {
        match result {
            Ok(Some(path)) => {
                match file {
                    EngineFile::Executable => self.config.engine_executable = path,
                    EngineFile::Model => {
                        self.config.nemo_model = Model::at(&path);
                        self.config.model = path;
                        if self.config.uses_nemotron() {
                            self.config.language =
                                crate::models::nemotron_language(&self.config.language).into();
                        }
                    },
                }
                self.edited();
            },
            Ok(None) => {},
            Err(_) => {
                self.notice = Some(Notice::Problem(
                    "Cannot open the file picker. Try again.".into(),
                ));
            },
        }
        self.file_picker = None;
        cx.notify();
    }

    pub(super) fn play(&mut self, cx: &Context<'_, Self>) {
        let services = cx.global::<Services>();
        let script = demo::play(
            services.output.clone(),
            services.meter.clone(),
            services.canvas.clone(),
        );
        self.preview = Some(cx.spawn(async move |_, _| script.await));
    }

    fn toggle_setup(&mut self, cx: &Context<'_, Self>) {
        match self.setup.take() {
            Some(setup) => self.pause_setup(setup, cx),
            None => self.start_setup(cx),
        }
    }

    pub(super) fn start_setup(&mut self, cx: &Context<'_, Self>) {
        self.install_model(Model::Parakeet, cx);
    }

    fn install_model(&mut self, model: Model, cx: &Context<'_, Self>) {
        if cx.global::<Services>().is_quitting()
            || is_demo(cx)
            || self.setup.is_some()
            || self.retiring_setup.is_some()
            || self.unreadable
            || self.resetting()
        {
            return;
        }
        self.practice.stop();
        self.language.cursor = None;
        let setup = match Setup::start(model) {
            Ok(setup) => setup,
            Err(error) => {
                self.notice = Some(Notice::Problem(error.to_string().into()));
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
        self.setup = Some((setup, follow_setup, self.config.clone()));
    }

    fn pause_setup(
        &mut self,
        (setup, follow_setup, _): (Setup, Task<()>, Config),
        cx: &Context<'_, Self>,
    ) {
        drop(follow_setup);
        setup.request_stop();
        let stopped = setup.stopped();
        let retirement = cx.spawn(async move |this, cx| {
            stopped.await;
            this.update_if_alive(cx, |view, cx| {
                view.retiring_setup = None;
                cx.notify();
            });
        });
        self.retiring_setup = Some((setup, retirement));
        self.notice = Some(Notice::Note(
            "Setup paused. Downloads resume where they stopped.".into(),
        ));
    }

    fn finish_setup(
        &mut self,
        outcome: Option<anyhow::Result<Installed>>,
        cx: &mut Context<'_, Self>,
    ) {
        let auto_apply = self.setup.take().is_some_and(|(_, _, started)| {
            started.needs_engine()
                && started == self.config
                && !self.vocabulary.pending(cx)
                && !self.recognition.pending(cx)
                && !self.presets.pending(cx)
        });
        self.downloaded = crate::setup::downloaded();
        match outcome {
            Some(Ok(installed)) => {
                installed.apply_to(&mut self.config);
                self.own_engine = false;
                if auto_apply {
                    self.save(cx);
                } else {
                    self.notice = Some(Notice::Note(
                        "Model ready. Save to use it. Your other changes remain in this draft."
                            .into(),
                    ));
                }
            },
            Some(Err(error)) => self.notice = Some(Notice::Problem(error.to_string().into())),
            None => {
                self.notice = Some(Notice::Problem(
                    "Setup stopped unexpectedly. Try again.".into(),
                ));
            },
        }
        cx.notify();
    }

    fn request_setup_stops(&self) {
        if let Some((setup, _, _)) = &self.setup {
            setup.request_stop();
        }
        if let Some((setup, _)) = &self.retiring_setup {
            setup.request_stop();
        }
    }

    fn save(&mut self, cx: &mut Context<'_, Self>) {
        self.practice.stop();
        if self.vocabulary.pending(cx) && !self.add_correction(cx) {
            return;
        }
        if self.recognition.pending(cx) && !self.add_recognition_term(cx) {
            return;
        }
        if self.presets.pending(cx) && !self.capture_preset(None, cx) {
            return;
        }
        let services = cx.global::<Services>();

        if services.is_quitting() || services.mode.is_demo() {
            return;
        }
        let epoch = services.config_epoch();
        self.notice = Some(Notice::Note("Saving changes…".into()));
        if let Some(progress) = self.saves.submit(self.config.clone(), epoch) {
            self.advance_save(progress, cx);
        }
        cx.notify();
    }

    fn advance_save(&mut self, progress: SaveProgress, cx: &mut Context<'_, Self>) {
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

    fn follow_write(
        result: Receiver<anyhow::Result<SavedConfig>>,
        cx: &Context<'_, Self>,
    ) -> Task<()> {
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
        cx: &mut Context<'_, Self>,
    ) {
        let Some(saved) = saved else {
            self.notice = error.map(|error| Notice::Problem(error.into()));
            return;
        };
        saved.update_draft(&mut self.config);
        let applied = cx.update_global::<Services, _>(|services, cx| {
            services.adopt_saved(saved.validated, epoch, cx)
        });
        self.notice = error
            .or_else(|| applied.err().map(|error| error.to_string()))
            .map(|error| Notice::Problem(error.into()));
        show_motion(cx.global::<Services>().config.prefers_reduced_motion(), cx);
        show_theme(cx);
    }

    pub(super) fn cancel_background_work(&mut self) {
        self.practice.stop();
        self.file_picker = None;
        self.microphone_listing = None;
        self.preview = None;
        self.request_setup_stops();
        self.update.cancel();
    }

    pub(super) fn drain_finished(&mut self) -> bool {
        let update_drained = self.update.drained();
        self.setup = self
            .setup
            .take()
            .filter(|(setup, _, _)| !setup.is_finished());
        self.retiring_setup = self
            .retiring_setup
            .take()
            .filter(|(setup, _)| !setup.is_finished());
        self.setup.is_none()
            && self.retiring_setup.is_none()
            && self.onboarding_completion.is_none()
            && self.saves.is_idle()
            && update_drained
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        self.update.cancel();
        self.request_setup_stops();

        self.saves.flush();
    }
}

enum Notice {
    Note(SharedString),
    Problem(SharedString),
}

enum Reset {
    Idle,
    Confirming,
    Running { _work: Task<()> },
}

fn update_notice(version: Option<crate::updates::Version>) -> Notice {
    let action = "Open release";
    #[cfg(target_os = "windows")]
    let action = if crate::updates::Download::available() {
        "Download update"
    } else {
        action
    };
    let message = match version {
        Some(version) => format!("Speakeasy {version} is available. Choose {action}."),
        None => "No newer release is available.".into(),
    };
    Notice::Note(message.into())
}

impl Notice {
    fn text(&self) -> SharedString {
        match self {
            Self::Note(text) | Self::Problem(text) => text.clone(),
        }
    }

    const fn is_problem(&self) -> bool {
        matches!(self, Self::Problem(_))
    }
}

#[derive(PartialEq, Eq)]
struct Presentation {
    indicator: Indicator,
    shortcut: Arc<str>,
    cancel_shortcut: Arc<str>,
    message: String,
    readiness: Readiness,
}

impl Presentation {
    fn new(snapshot: &Snapshot, services: &Services) -> Self {
        let status = services.status(snapshot);
        Self {
            indicator: status.indicator,
            readiness: Readiness::new(snapshot, services.service_state()),
            shortcut: Arc::clone(&snapshot.shortcut),
            cancel_shortcut: Arc::clone(&snapshot.cancel_shortcut),
            message: if snapshot.message.is_empty() {
                status.description.into_owned()
            } else {
                snapshot.message.clone()
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Onboarding(onboarding::Action),
    Page(navigation::Page),
    Revert,
    #[cfg(target_os = "macos")]
    MacShortcut,
    Install(Model),
    SelectLanguage(&'static str),
    CheckUpdates,
    OpenUpdate,
    #[cfg(target_os = "windows")]
    DownloadUpdate,
    #[cfg(target_os = "windows")]
    CancelUpdate,
    #[cfg(target_os = "windows")]
    InstallUpdate,
    Practice,
    PermissionHelp(permissions::Help),
    Shows,
    Setup,
    OwnEngine,
    Engine(Engine),
    Choose(EngineFile),
    Reset,
    ConfirmReset,
    CancelReset,
    Microphone,
    Refresh,
    Language,
    Delivery,
    Toggle(Setting),
    Theme,
    Save,
    Resume,
    Pause,
    Preview,
    Quit,
}

impl Action {
    const fn element_id(self) -> &'static str {
        match self {
            Self::Onboarding(onboarding::Action::Replay) => "replay-setup",
            Self::Onboarding(onboarding::Action::Next) => "onboarding-next",
            Self::Onboarding(onboarding::Action::Back) => "onboarding-back",
            Self::Onboarding(onboarding::Action::Finish) => "onboarding-finish",
            Self::Page(navigation::Page::Dictation) => "page-dictation",
            Self::Page(navigation::Page::Words) => "page-words",
            Self::Page(navigation::Page::Advanced) => "page-advanced",
            Self::Revert => "revert",
            #[cfg(target_os = "macos")]
            Self::MacShortcut => "mac-shortcut",
            Self::Install(Model::Parakeet) => "install-parakeet",
            Self::Install(Model::Nemotron) => "install-nemotron",
            Self::SelectLanguage(code) => code,
            Self::CheckUpdates => "check-updates",
            Self::OpenUpdate => "open-update",
            #[cfg(target_os = "windows")]
            Self::DownloadUpdate => "download-update",
            #[cfg(target_os = "windows")]
            Self::CancelUpdate => "cancel-update",
            #[cfg(target_os = "windows")]
            Self::InstallUpdate => "install-update",
            Self::Practice => "practice",
            Self::PermissionHelp(permissions::Help::Microphone) => "microphone-help",
            #[cfg(target_os = "macos")]
            Self::PermissionHelp(permissions::Help::Accessibility) => "accessibility-help",
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::PermissionHelp(permissions::Help::Screen) => "screen-help",
            Self::Shows => "shows",
            Self::Setup => "setup",
            Self::OwnEngine => "own-engine",
            Self::Engine(Engine::Whisper) => "engine-whisper",
            Self::Engine(Engine::Nemo) => "engine-nemo",
            Self::Reset => "reset",
            Self::ConfirmReset => "confirm-reset",
            Self::CancelReset => "cancel-reset",
            Self::Choose(EngineFile::Executable) => "choose-executable",
            Self::Choose(EngineFile::Model) => "choose-model",
            Self::Microphone => "microphone",
            Self::Refresh => "refresh",
            Self::Language => "language",
            Self::Delivery => "delivery",
            Self::Toggle(Setting::PreferGpu) => "prefer-gpu",
            Self::Toggle(Setting::ReduceMotion) => "reduce-motion",
            Self::Toggle(Setting::RemoveFillers) => "remove-fillers",
            Self::Toggle(Setting::PlaySounds) => "play-sounds",
            Self::Toggle(Setting::DesktopBindings) => "desktop-bindings",
            Self::Toggle(Setting::StartAtLogin) => "start-at-login",
            Self::Theme => "theme",
            Self::Save | Self::Resume => "primary",
            Self::Pause => "pause",
            Self::Preview => "preview",
            Self::Quit => "quit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngineFile {
    Executable,
    Model,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setting {
    PreferGpu,
    ReduceMotion,
    RemoveFillers,
    PlaySounds,
    DesktopBindings,
    StartAtLogin,
}

impl Setting {
    fn flag(self, config: &mut Config) -> &mut bool {
        match self {
            Self::PreferGpu => &mut config.use_gpu,
            Self::ReduceMotion => &mut config.reduced_motion,
            Self::RemoveFillers => &mut config.remove_fillers,
            Self::PlaySounds => &mut config.play_sounds,
            Self::DesktopBindings => &mut config.linux.external_shortcut,
            Self::StartAtLogin => &mut config.start_at_login,
        }
    }

    fn is_on(self, config: &Config) -> bool {
        match self {
            Self::PreferGpu => config.use_gpu,
            Self::ReduceMotion => config.reduced_motion,
            Self::RemoveFillers => config.remove_fillers,
            Self::PlaySounds => config.play_sounds,
            Self::DesktopBindings => config.linux.external_shortcut,
            Self::StartAtLogin => config.start_at_login,
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn desktop_command(path: &std::path::Path, default: bool) -> String {
    if default {
        return "speakeasy".into();
    }
    let quoted = path.to_string_lossy().replace('\'', r"'\''");
    format!("speakeasy --config '{quoted}'")
}

fn is_demo(cx: &App) -> bool {
    cx.global::<Services>().mode.is_demo()
}

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

fn show_theme(cx: &mut App) {
    let services = cx.global::<Services>();
    let (pill, theme) = (services.pill, services.config.theme);
    pill.update_if_open(cx, |view, _, cx| view.set_theme(theme, cx));
    if let Some(shows) = cx.global::<Services>().shows {
        shows.update_if_open(cx, |view, _, cx| view.set_theme(theme, cx));
    }

    cx.global::<Services>().output.send_modify(|_| ());
}

fn show_motion(reduced_motion: bool, cx: &mut App) {
    let pill = cx.global::<Services>().pill;
    pill.update_if_open(cx, |view, _, cx| {
        view.set_reduced_motion(reduced_motion, cx);
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

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
    fn desktop_commands_name_only_a_settings_file_other_than_the_default() {
        let default = Path::new("/home/user/.config/speakeasy/settings.json");
        assert_eq!(desktop_command(default, true), "speakeasy");
        assert_eq!(
            desktop_command(Path::new("/home/user/it's mine.json"), false),
            r"speakeasy --config '/home/user/it'\''s mine.json'"
        );
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod native_tests;

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod native_interaction;

#[cfg(all(test, target_os = "windows"))]
mod native_accessibility;
