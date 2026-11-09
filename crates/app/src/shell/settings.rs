mod menu;
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

use std::{mem, path::PathBuf, sync::Arc, time::Duration};

use async_channel::Receiver;
use gpui::{App, BorrowAppContext, Context, ElementId, SharedString, Task, Window};
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
    config::{Config, Engine, TextDelivery},
    gpui_ext::{EntityUpdate, WindowUpdate, follow},
    models::Model,
    runtime::{ModelState, Snapshot},
    setup::{Installed, Setup},
    status::Indicator,
    theme::Theme,
};

const PROGRESS_THROTTLE: Duration = Duration::from_millis(100);

const SAVED_FLASH: Duration = Duration::from_millis(1_600);

#[expect(
    clippy::struct_excessive_bools,
    reason = "Settings owns independent window flags: file readability, the own-engine choice, disclosures, input modality, and the Windows startup approval it observes"
)]
pub(super) struct Settings {
    config: Config,
    presentation: Presentation,
    permissions: Option<Permissions>,
    notice: Option<Notice>,
    microphones: Vec<menu::Offered>,
    file_picker: Option<Task<()>>,
    microphone_listing: Option<Task<()>>,
    preview: Option<Task<()>>,
    download: Download,
    saving: Option<Task<()>>,
    saves: SaveQueue,
    saved: Option<Task<()>>,
    vocabulary: vocabulary::Editor,
    recognition: recognition::Editor,
    presets: presets::Editor,
    menus: menu::Menus,
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
    choosing_model: bool,
    shows_help: bool,
    keyboard: bool,
    startup_apps_off: bool,
    #[cfg(target_os = "linux")]
    desktop_command: SharedString,
    #[cfg(target_os = "windows")]
    pub(super) minimize_to_tray: Option<Task<()>>,
}

struct Installing {
    model: Model,
    setup: Setup,
    _follow: Task<()>,
}

enum Download {
    Idle,
    Installing(Installing),
    Pausing { setup: Setup, _retirement: Task<()> },
}

impl Download {
    const fn is_busy(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    const fn is_pausing(&self) -> bool {
        matches!(self, Self::Pausing { .. })
    }

    const fn installing(&self) -> Option<&Installing> {
        match self {
            Self::Installing(installing) => Some(installing),
            Self::Idle | Self::Pausing { .. } => None,
        }
    }

    const fn setup(&self) -> Option<&Setup> {
        match self {
            Self::Installing(Installing { setup, .. }) | Self::Pausing { setup, .. } => Some(setup),
            Self::Idle => None,
        }
    }
}

impl Settings {
    pub(super) fn new(
        config: Config,
        mut updates: watch::Receiver<Snapshot>,
        window: &mut Window,
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
        let vocabulary = vocabulary::Editor::new(config.theme, window, cx);
        let recognition = recognition::Editor::new(config.theme, window, cx);
        let presets = presets::Editor::new(config.theme, window, cx);
        let mut view = Self {
            config,
            presentation,
            permissions: None,
            notice: None,
            microphones: Vec::new(),
            file_picker: None,
            microphone_listing: None,
            preview: None,
            download: Download::Idle,
            saving: None,
            saves,
            saved: None,
            vocabulary,
            recognition,
            presets,
            menus: menu::Menus::new(cx),
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
            choosing_model: false,
            shows_help: false,
            keyboard: false,
            startup_apps_off: false,
            #[cfg(target_os = "linux")]
            desktop_command,
            #[cfg(target_os = "windows")]
            minimize_to_tray: None,
        };
        view.refresh_microphones(cx);
        super::report_focus(window, cx);
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
        if !matches!(action, Action::Menu(_)) {
            self.close_menu(window, cx);
        }
        match action {
            Action::Onboarding(action) => self.onboarding_action(action, window, cx),
            Action::Page(page) => self.open_page(page, window, cx),
            Action::Menu(menu) => self.toggle_menu(menu, window, cx),
            Action::Setup => self.toggle_setup(cx),
            Action::Install(model) => self.install_model(model, cx),
            Action::Models => self.choosing_model = !self.choosing_model,
            Action::ChooseModel => {
                self.choosing_model = true;
                self.open_page(navigation::Page::Dictation, window, cx);
            },
            Action::ShowsHelp => self.shows_help = !self.shows_help,
            Action::OwnEngine => self.use_own_engine(cx),
            Action::Engine(engine) => self.switch_engine(engine, cx),
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
            Action::SelectMicrophone(index) => {
                let chosen = match index {
                    None => Some(None),
                    Some(index) => self
                        .microphones
                        .get(index)
                        .filter(|row| row.present)
                        .map(|row| Some(row.device.id.clone())),
                };
                if let Some(microphone) = chosen {
                    self.config.microphone = microphone;
                    self.edited(cx);
                }
            },
            Action::SelectLanguage(language) => {
                self.config.language = if self.config.engine == Engine::Whisper {
                    crate::models::whisper_language(language)
                } else {
                    language
                }
                .into();
                self.edited(cx);
            },
            Action::SelectDelivery(delivery) => {
                self.config.text_delivery = delivery;
                self.edited(cx);
            },
            #[cfg(target_os = "macos")]
            Action::SelectShortcut(shortcut) => {
                self.config.mac_shortcut = shortcut;
                self.edited(cx);
            },
            #[cfg(target_os = "linux")]
            Action::SelectBindings(external) => {
                self.config.linux.external_shortcut = external;
                self.edited(cx);
            },
            Action::Toggle(setting) => self.toggle(setting, cx),
            Action::Theme(theme) => self.choose_theme(theme, cx),
            Action::Preset(edit) => self.edit_preset(edit, window, cx),
            Action::Correction(edit) => self.edit_correction(edit, window, cx),
            Action::Hint(edit) => self.edit_hint(edit, window, cx),
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
            Action::CancelUpdate => self.cancel_update(),
            #[cfg(target_os = "windows")]
            Action::InstallUpdate => self.install_update(cx),
            Action::PermissionHelp(help) => cx.open_url(help.url()),
            Action::Shows => super::reveal_shows(cx),
            Action::Quit => request_quit(cx),
        }
        cx.notify();
    }

    fn use_own_engine(&mut self, cx: &mut Context<'_, Self>) {
        speech::use_own_engine(&mut self.config);
        self.own_engine = true;
        self.edited(cx);
    }

    fn switch_engine(&mut self, engine: Engine, cx: &mut Context<'_, Self>) {
        if speech::switch_engine(&mut self.config, engine) {
            self.edited(cx);
        }
    }

    const fn resetting(&self) -> bool {
        matches!(self.reset, Reset::Running { .. })
    }

    fn can_reset(&self, cx: &App) -> bool {
        let services = cx.global::<Services>();
        !services.mode.is_demo()
            && !services.is_quitting()
            && !self.download.is_busy()
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
                self.menus.dismiss_presets();
                self.own_engine = false;
                self.choosing_model = false;
                self.file_picker = None;
                present_appearance(cx);
                self.start_setup(cx);
            },
            Err(error) => self.notice = Some(Notice::Problem(error.into())),
        }
        cx.notify();
    }

    fn edited(&mut self, cx: &mut Context<'_, Self>) {
        self.practice.stop();
        self.notice = None;
        self.commit(cx);
    }

    fn commit(&mut self, cx: &mut Context<'_, Self>) {
        let services = cx.global::<Services>();
        if services.is_quitting()
            || services.mode.is_demo()
            || self.unreadable
            || self.resetting()
            || !self.config_complete()
            || !self.saves.needs_save(&self.config)
        {
            return;
        }
        let epoch = services.config_epoch();
        if let Some(progress) = self.saves.submit(self.config.clone(), epoch) {
            self.advance_save(progress, cx);
        }
    }

    fn config_complete(&self) -> bool {
        !self.config.needs_engine() && !self.config.model.as_os_str().is_empty()
    }

    fn unsaved(&self, cx: &App) -> bool {
        !is_demo(cx) && self.saves.needs_save(&self.config)
    }

    fn toggle(&mut self, setting: Setting, cx: &mut Context<'_, Self>) {
        let on = !setting.is_on(&self.config);
        *setting.flag(&mut self.config) = on;
        if setting == Setting::RemoveFillers && speech::detects_language(&self.config) {
            self.config.language = if on { "en" } else { "auto" }.into();
        }
        self.edited(cx);
        if is_demo(cx) && setting == Setting::ReduceMotion {
            cx.global_mut::<Services>().config.reduced_motion = self.config.reduced_motion;
            present_appearance(cx);
        }
    }

    fn choose_theme(&mut self, theme: Theme, cx: &mut Context<'_, Self>) {
        self.config.theme = theme;
        self.edited(cx);
        if is_demo(cx) {
            cx.global_mut::<Services>().config.theme = theme;
            present_appearance(cx);
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
                view.microphones_listed(result, cx);
            });
        }));
    }

    fn microphones_listed(
        &mut self,
        result: anyhow::Result<Vec<InputDevice>>,
        cx: &mut Context<'_, Self>,
    ) {
        match result {
            Ok(microphones) => {
                if self.menus.open != Some(menu::Menu::Microphone) {
                    self.microphones.clear();
                }
                menu::keep_rows(&mut self.microphones, microphones);
            },
            Err(_) => {
                self.notice = Some(Notice::Problem(
                    "Cannot list microphones. Check your system's sound settings.".into(),
                ));
            },
        }
        self.microphone_listing = None;
        cx.notify();
    }

    fn choose(&mut self, file: EngineFile, window: &Window, cx: &Context<'_, Self>) {
        if self.file_picker.is_some() {
            return;
        }
        let programs = FileFilter {
            name: "Programs",
            pattern: "*.exe",
        };
        let (title, filter) = match (self.config.engine, file) {
            (Engine::Nemo, EngineFile::Executable) => ("Choose nemo-speech", programs),
            (Engine::Whisper, EngineFile::Executable) => ("Choose whisper-server", programs),
            (Engine::Nemo, EngineFile::Model) => (
                "Choose a NeMo GGUF model",
                FileFilter {
                    name: "NeMo models",
                    pattern: "*.gguf",
                },
            ),
            (Engine::Whisper, EngineFile::Model) => (
                "Choose a Whisper GGML model",
                FileFilter {
                    name: "Whisper models",
                    pattern: "*.bin",
                },
            ),
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
        self.file_picker = None;
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
                self.edited(cx);
            },
            Ok(None) => {},
            Err(_) => {
                self.notice = Some(Notice::Problem(
                    "Cannot open the file picker. Try again.".into(),
                ));
            },
        }
        cx.notify();
    }

    pub(super) fn play(&mut self, cx: &Context<'_, Self>) {
        let services = cx.global::<Services>();
        let script = demo::Script::new(
            cx.background_executor().clone(),
            services.output.clone(),
            services.meter.clone(),
            services.canvas.clone(),
        );
        self.preview = Some(cx.spawn(async move |_, _| script.play().await));
    }

    fn toggle_setup(&mut self, cx: &Context<'_, Self>) {
        match mem::replace(&mut self.download, Download::Idle) {
            Download::Installing(Installing { setup, .. }) => self.pause_setup(setup, cx),
            Download::Idle => self.start_setup(cx),
            pausing @ Download::Pausing { .. } => self.download = pausing,
        }
    }

    pub(super) fn start_setup(&mut self, cx: &Context<'_, Self>) {
        self.install_model(Model::Parakeet, cx);
    }

    fn install_model(&mut self, model: Model, cx: &Context<'_, Self>) {
        if cx.global::<Services>().is_quitting()
            || is_demo(cx)
            || self.download.is_busy()
            || self.unreadable
            || self.resetting()
        {
            return;
        }
        self.practice.stop();
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
                cx.background_executor().timer(PROGRESS_THROTTLE).await;
            }
            let outcome = outcome.await;
            this.update_if_alive(cx, |view, cx| view.finish_setup(outcome, cx));
        });
        self.download = Download::Installing(Installing {
            model,
            setup,
            _follow: follow_setup,
        });
    }

    fn pause_setup(&mut self, setup: Setup, cx: &Context<'_, Self>) {
        setup.request_stop();
        let stopped = setup.stopped();
        let retirement = cx.spawn(async move |this, cx| {
            stopped.await;
            this.update_if_alive(cx, |view, cx| {
                if view.download.is_pausing() {
                    view.download = Download::Idle;
                }
                cx.notify();
            });
        });
        self.download = Download::Pausing {
            setup,
            _retirement: retirement,
        };
        self.notice = Some(Notice::Note(
            "Download paused. It resumes where it stopped.".into(),
        ));
    }

    fn finish_setup(
        &mut self,
        outcome: Option<anyhow::Result<Installed>>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.download.installing().is_some() {
            self.download = Download::Idle;
        }
        self.downloaded = crate::setup::downloaded();
        match outcome {
            Some(Ok(installed)) => {
                installed.apply_to(&mut self.config);
                self.own_engine = false;
                self.edited(cx);
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
        if let Some(setup) = self.download.setup() {
            setup.request_stop();
        }
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
                    "Saving settings stopped unexpectedly. Change the setting again to retry."
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
        let applied = saved.map(|saved| {
            saved.update_draft(&mut self.config);
            cx.update_global::<Services, _>(|services, cx| {
                services.adopt_saved(saved.validated, epoch, cx)
            })
        });
        if error.is_some() {
            let restored = cx.global::<Services>().config.clone();
            if restored.presets != self.config.presets {
                self.menus.dismiss_presets();
            }
            self.config = restored;
            self.own_engine = false;
            self.vocabulary.stop_revising();
        }
        self.notice = error
            .or_else(|| applied?.err().map(|error| error.to_string()))
            .map(|error| Notice::Problem(error.into()));
        if self.notice.is_none() {
            self.flash_saved(cx);
        }
        present_appearance(cx);
    }

    fn flash_saved(&mut self, cx: &Context<'_, Self>) {
        self.saved = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVED_FLASH).await;
            this.update_if_alive(cx, |view, cx| {
                view.saved = None;
                cx.notify();
            });
        }));
    }

    pub(super) fn cancel_background_work(&mut self) {
        self.practice.stop();
        self.file_picker = None;
        self.microphone_listing = None;
        self.preview = None;
        self.request_setup_stops();
        self.update.cancel();
    }

    pub(super) fn drained(&mut self) -> bool {
        let update_drained = self.update.drained();
        if self.download.setup().is_some_and(Setup::is_finished) {
            self.download = Download::Idle;
        }
        !self.download.is_busy()
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
    model: Option<ModelState>,
}

impl Presentation {
    fn new(snapshot: &Snapshot, services: &Services) -> Self {
        let status = services.status(snapshot);
        Self {
            indicator: status.indicator,
            model: services.is_running().then_some(snapshot.model),
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
    Menu(menu::Menu),
    Setup,
    Install(Model),
    Models,
    ChooseModel,
    ShowsHelp,
    OwnEngine,
    Engine(Engine),
    Choose(EngineFile),
    SelectLanguage(&'static str),
    SelectMicrophone(Option<usize>),
    SelectDelivery(TextDelivery),
    #[cfg(target_os = "macos")]
    SelectShortcut(speakeasy_platform::MacShortcut),
    #[cfg(target_os = "linux")]
    SelectBindings(bool),
    Toggle(Setting),
    Theme(Theme),
    Preset(presets::Edit),
    Correction(vocabulary::Edit),
    Hint(recognition::Edit),
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
    Reset,
    ConfirmReset,
    CancelReset,
    Resume,
    Pause,
    Preview,
    Shows,
    Quit,
}

impl Action {
    fn element_id(self) -> ElementId {
        SharedString::from(format!("{self:?}")).into()
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
    StartAtLogin,
}

impl Setting {
    fn flag(self, config: &mut Config) -> &mut bool {
        match self {
            Self::PreferGpu => &mut config.use_gpu,
            Self::ReduceMotion => &mut config.reduced_motion,
            Self::RemoveFillers => &mut config.remove_fillers,
            Self::PlaySounds => &mut config.play_sounds,
            Self::StartAtLogin => &mut config.start_at_login,
        }
    }

    fn is_on(self, config: &Config) -> bool {
        match self {
            Self::PreferGpu => config.use_gpu,
            Self::ReduceMotion => config.reduced_motion,
            Self::RemoveFillers => {
                config.fillers(&config.language) == crate::transcript::Fillers::Remove
            },
            Self::PlaySounds => config.play_sounds,
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

fn present_appearance(cx: &mut App) {
    let services = cx.global::<Services>();
    let (pill, shows) = (services.pill, services.shows);
    let theme = services.config.theme;
    let reduced_motion = services.config.prefers_reduced_motion();
    pill.update_if_open(cx, |view, _, cx| {
        view.set_theme(theme, cx);
        view.set_reduced_motion(reduced_motion, cx);
    });
    if let Some(shows) = shows {
        shows.update_if_open(cx, |view, _, cx| view.set_theme(theme, cx));
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

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
