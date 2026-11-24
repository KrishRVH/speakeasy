pub(crate) mod presets;

use std::{
    fs,
    io::{self, ErrorKind, Write as _},
    ops::Deref,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use speakeasy_platform::{
    Binding, Delivery, DesktopOptions, LinuxPaste, MacShortcut, NativeShortcut, Problem, Refusal,
    Scope, Shortcut, Shortcuts, Toggle, Toggles, WindowsShortcut,
};

use crate::{
    models::{self, Model},
    recognition_terms::Terms,
    theme::Theme,
    transcript::{
        Fillers, Writing,
        symbols::Symbols,
        vocabulary::{self, Correction},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Engine {
    Whisper,
    Nemo,
}

impl Engine {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Whisper => "Whisper",
            Self::Nemo => "NeMo",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each flag is an independent saved preference; every combination is valid"
)]
pub(crate) struct Config {
    #[serde(skip_serializing_if = "LinuxSettings::is_default")]
    pub linux: LinuxSettings,
    /// Hold to talk; settings files carry this key, so the name stays.
    pub mac_shortcut: MacShortcut,
    #[serde(skip_serializing_if = "Toggles::is_empty")]
    pub mac_toggles: Toggles<MacShortcut>,
    /// Hold to talk; settings files carry this key, so the name stays.
    pub windows_shortcut: WindowsShortcut,
    #[serde(skip_serializing_if = "Toggles::is_empty")]
    pub windows_toggles: Toggles<WindowsShortcut>,
    pub engine: Engine,
    pub microphone: Option<String>,
    pub engine_executable: PathBuf,
    pub model: PathBuf,
    pub nemo_model: Option<Model>,
    pub language: String,
    pub recognition_terms: Terms,
    pub remove_fillers: bool,
    pub spoken_symbols: bool,
    pub print_to_scripts: bool,
    pub vocabulary: Vec<Correction>,
    pub presets: Vec<presets::Preset>,
    pub threads: u16,
    pub use_gpu: bool,
    pub reduced_motion: bool,
    pub text_delivery: TextDelivery,
    pub play_sounds: bool,
    pub theme: Theme,
    pub start_at_login: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            linux: LinuxSettings::default(),
            mac_shortcut: MacShortcut::default(),
            mac_toggles: Toggles::default(),
            windows_shortcut: WindowsShortcut::default(),
            windows_toggles: Toggles::default(),
            engine: Engine::Nemo,
            microphone: None,
            engine_executable: PathBuf::new(),
            model: PathBuf::new(),
            nemo_model: None,
            language: "auto".into(),
            recognition_terms: Terms::default(),
            remove_fillers: true,
            spoken_symbols: true,
            print_to_scripts: false,
            vocabulary: Vec::new(),
            presets: Vec::new(),
            threads: 4,
            use_gpu: true,
            reduced_motion: false,
            text_delivery: TextDelivery::default(),
            play_sounds: true,
            theme: Theme::default(),
            start_at_login: false,
        }
    }
}

/// The Settings row that names a shortcut.
pub(crate) const fn binding_title(binding: Binding) -> &'static str {
    match binding {
        Binding::Talk => "Hold to talk",
        Binding::Toggle(Toggle::HandsFree) => "Hands-free",
        Binding::Toggle(Toggle::Show(Scope::Screen)) => "Screen Show",
        Binding::Toggle(Toggle::Show(Scope::Window)) => "Window Show",
        Binding::Toggle(Toggle::Show(Scope::Area)) => "Area Show",
    }
}

/// Why Settings refuses keys for a shortcut, beside the hold-to-talk keys `talk`.
pub(crate) fn refusal_message(refusal: Refusal, talk: NativeShortcut) -> String {
    let (modifiers, reserved) = if cfg!(target_os = "macos") {
        ("Control or Command", "Esc and Caps Lock")
    } else {
        ("Ctrl, Alt, or Win", "Esc, Caps Lock, and Num Lock")
    };
    match refusal {
        Refusal::Problem(Problem::Types) => format!("These keys type text. Add {modifiers}."),
        Refusal::Problem(Problem::Reserved) => format!("{reserved} can't be shortcuts."),
        Refusal::Problem(Problem::ShiftAlone) => {
            "Shift alone types capitals. Add another key.".into()
        },
        Refusal::Problem(Problem::AltGraph) => {
            "Many keyboards type characters with Ctrl and Alt together, as AltGr. Choose other keys."
                .into()
        },
        Refusal::NeedsKey => {
            "Add a key to the modifiers. Only hold to talk works with modifiers alone.".into()
        },
        Refusal::Taken(binding) => format!("{} already uses these keys.", binding_title(binding)),
        Refusal::BehindTalk => format!(
            "These keys include {}, which starts dictation first.",
            talk.label()
        ),
        Refusal::Covers(toggle) => format!(
            "{} uses these keys too, so pressing it would start dictation instead.",
            binding_title(Binding::Toggle(toggle))
        ),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LinuxSettings {
    pub shortcut: String,
    pub cancel: String,
    pub external_shortcut: bool,
}

impl LinuxSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for LinuxSettings {
    fn default() -> Self {
        let defaults = DesktopOptions::default();
        Self {
            shortcut: defaults.shortcut,
            cancel: defaults.cancel,
            external_shortcut: defaults.external_shortcut,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TextDelivery {
    #[default]
    Paste,
    #[cfg(target_os = "linux")]
    TerminalPaste,
    Type,
    #[cfg(target_os = "linux")]
    Copy,
}

impl TextDelivery {
    const fn linux_paste(self) -> LinuxPaste {
        match self {
            Self::Paste | Self::Type => LinuxPaste::Standard,
            #[cfg(target_os = "linux")]
            Self::TerminalPaste => LinuxPaste::Terminal,
            #[cfg(target_os = "linux")]
            Self::Copy => LinuxPaste::Manual,
        }
    }
}

impl Config {
    pub(crate) fn fillers(&self, language: &str) -> Fillers {
        if self.remove_fillers && models::is_english(language) {
            Fillers::Remove
        } else {
            Fillers::Preserve
        }
    }

    pub(crate) fn writing(&self) -> Writing {
        Writing {
            fillers: self.fillers(&self.language),
            symbols: if self.spoken_symbols {
                Symbols::Write
            } else {
                Symbols::Keep
            },
        }
    }

    pub(crate) fn uses_nemotron(&self) -> bool {
        self.model_choice() == Some(Model::Nemotron)
    }

    pub(crate) fn hears_hints(&self) -> bool {
        self.engine == Engine::Whisper || self.uses_nemotron()
    }

    pub(crate) fn model_choice(&self) -> Option<Model> {
        managed_model(self.engine, &self.model, self.nemo_model)
    }

    pub(crate) fn speech_label(&self) -> &'static str {
        self.model_choice()
            .map_or_else(|| self.engine.label(), Model::label)
    }

    pub(crate) fn delivery(&self) -> Delivery {
        match self.text_delivery {
            TextDelivery::Type => Delivery::Direct,
            TextDelivery::Paste => Delivery::Paste,
            #[cfg(target_os = "linux")]
            TextDelivery::TerminalPaste | TextDelivery::Copy => Delivery::Paste,
        }
    }

    pub(crate) fn prefers_reduced_motion(&self) -> bool {
        self.reduced_motion || speakeasy_platform::reduced_motion()
    }

    pub(crate) const fn shortcut(&self) -> Shortcut {
        Shortcut {
            mac: self.mac_shortcut,
            windows: self.windows_shortcut,
        }
    }

    /// This system's shortcuts; Linux binds its own through the desktop.
    pub(crate) const fn shortcuts(&self) -> Shortcuts<NativeShortcut> {
        #[cfg(target_os = "macos")]
        return self.mac_shortcuts();
        #[cfg(not(target_os = "macos"))]
        return self.windows_shortcuts();
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) const fn set_shortcuts(&mut self, shortcuts: Shortcuts<NativeShortcut>) {
        #[cfg(target_os = "macos")]
        {
            self.mac_shortcut = shortcuts.talk;
            self.mac_toggles = shortcuts.toggles;
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.windows_shortcut = shortcuts.talk;
            self.windows_toggles = shortcuts.toggles;
        }
    }

    const fn mac_shortcuts(&self) -> Shortcuts<MacShortcut> {
        Shortcuts {
            talk: self.mac_shortcut,
            toggles: self.mac_toggles,
        }
    }

    const fn windows_shortcuts(&self) -> Shortcuts<WindowsShortcut> {
        Shortcuts {
            talk: self.windows_shortcut,
            toggles: self.windows_toggles,
        }
    }

    pub(crate) fn desktop_options(&self) -> DesktopOptions {
        DesktopOptions {
            mac: self.mac_shortcuts(),
            windows: self.windows_shortcuts(),
            shortcut: self.linux.shortcut.clone(),
            cancel: self.linux.cancel.clone(),
            paste: self.text_delivery.linux_paste(),
            external_shortcut: self.linux.external_shortcut,
        }
    }

    pub(crate) fn read(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read(path).with_context(|| format!("Can't read {}", path.display()))?;
        Ok(parse(&contents, path)?.0)
    }

    pub(crate) fn is_absent(path: &Path) -> bool {
        matches!(path.try_exists(), Ok(false))
    }

    /// Why an existing settings file can't be used: the cause alone, since callers name the file.
    pub(crate) fn unreadable(path: &Path) -> Option<String> {
        if Self::is_absent(path) {
            return None;
        }
        Self::read(path)
            .err()
            .map(|error| error.root_cause().to_string())
    }

    pub(crate) fn discard(path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            removed => removed,
        }
    }

    pub(crate) fn load(path: &Path) -> (Self, Startup) {
        if Self::is_absent(path) {
            return (Self::default(), Startup::Unconfigured);
        }
        let config = match Self::read(path) {
            Ok(config) => config,
            Err(error) => return (Self::default(), Startup::Invalid(format!("{error:#}"))),
        };
        if config.needs_engine() {
            return (config, Startup::Unconfigured);
        }
        match config.validated(path) {
            Ok(valid) => (Self::clone(&valid), Startup::Configured(Box::new(valid))),
            Err(error) => (config, Startup::Invalid(format!("{error:#}"))),
        }
    }

    pub(crate) fn held_by(path: &Path, draft: &Self) -> Self {
        if Self::is_absent(path) {
            Self::default()
        } else {
            draft.clone()
        }
    }

    pub(crate) fn needs_engine(&self) -> bool {
        self.engine_executable.as_os_str().is_empty()
    }

    pub(crate) fn speech_configured(&self) -> bool {
        !self.needs_engine() && !self.model.as_os_str().is_empty()
    }

    pub(crate) fn change_to(&self, next: &Self) -> Change {
        // Every field is named, so a new one cannot compile until it is classified below.
        let Self {
            linux,
            mac_shortcut,
            mac_toggles,
            windows_shortcut,
            windows_toggles,
            engine,
            microphone,
            engine_executable,
            model,
            nemo_model,
            language,
            recognition_terms,
            remove_fillers,
            spoken_symbols,
            print_to_scripts,
            vocabulary,
            presets,
            threads,
            use_gpu,
            reduced_motion,
            text_delivery,
            play_sounds,
            theme,
            start_at_login,
        } = next;
        [
            (
                (cfg!(target_os = "linux")
                    && (self.linux != *linux
                        || self.text_delivery.linux_paste() != text_delivery.linux_paste()))
                    || (cfg!(target_os = "macos")
                        && (self.mac_shortcut != *mac_shortcut
                            || self.mac_toggles != *mac_toggles))
                    || (cfg!(target_os = "windows")
                        && (self.windows_shortcut != *windows_shortcut
                            || self.windows_toggles != *windows_toggles)),
                Change::Desktop,
            ),
            (
                self.engine != *engine
                    || self.engine_executable != *engine_executable
                    || self.model != *model
                    || self.nemo_model != *nemo_model
                    || self.threads != *threads
                    || self.use_gpu != *use_gpu,
                Change::Engine,
            ),
            (
                // Whisper's launch `--language` is only a default; each request names its own.
                self.language != *language
                    || self.microphone != *microphone
                    || self.recognition_terms != *recognition_terms
                    || self.remove_fillers != *remove_fillers
                    || self.spoken_symbols != *spoken_symbols
                    || self.vocabulary != *vocabulary
                    || self.text_delivery != *text_delivery
                    || self.play_sounds != *play_sounds
                    || self.print_to_scripts != *print_to_scripts,
                Change::Session,
            ),
            (
                self.theme != *theme
                    || self.reduced_motion != *reduced_motion
                    || self.start_at_login != *start_at_login
                    || self.presets != *presets,
                Change::Presentation,
            ),
        ]
        .into_iter()
        .filter_map(|(changed, tier)| changed.then_some(tier))
        .max()
        .unwrap_or(Change::Unchanged)
    }

    pub(crate) fn validated(&self, path: &Path) -> anyhow::Result<ValidConfig> {
        self.validate_choices()?;
        presets::validate(&self.presets)?;
        let mut config = self.clone();
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        for file in [&mut config.engine_executable, &mut config.model] {
            ensure!(
                !file.as_os_str().is_empty(),
                "Choose the engine executable and a matching model."
            );
            if file.is_relative() {
                *file = directory.join(&file);
            }
            *file = file
                .canonicalize()
                .map_err(|error| cannot_open(file, &error))?;
            ensure!(
                file.is_file(),
                "Not a file: {}. Choose the engine executable and model in Settings.",
                file.display()
            );
        }
        Ok(ValidConfig(config))
    }

    fn validate_choices(&self) -> anyhow::Result<()> {
        let shortcuts = self.shortcuts();
        if let Some((binding, refusal)) = shortcuts.refused() {
            return Err(anyhow!(
                "Choose another {} shortcut in Settings. {}",
                binding_title(binding),
                refusal_message(refusal, shortcuts.talk)
            ));
        }
        vocabulary::validate(&self.vocabulary)?;
        self.recognition_terms.validate()?;
        if self.uses_nemotron() {
            ensure!(
                models::LANGUAGES
                    .iter()
                    .any(|(code, _)| *code == self.language),
                "Choose a supported Nemotron language in Settings, or Detect automatically."
            );
        }
        ensure!(
            (1..=256).contains(&self.threads),
            "Set threads in the settings file to a number from 1 to 256."
        );
        ensure!(
            (1..=16).contains(&self.language.len()),
            "Set language in the settings file to a language code, such as en, or to auto."
        );
        Ok(())
    }

    /// Patches only the keys Settings changed, so hand edits made while Speakeasy runs survive.
    ///
    /// A key counts as unchanged when the draft matches any of `held`, each spelling the file may
    /// hold, such as a submitted draft and the paths validation resolved from it.
    pub(crate) fn save_changes(
        held: &[&Self],
        draft: &Self,
        validated: &ValidConfig,
        path: &Path,
    ) -> anyhow::Result<()> {
        let cannot_save = |error: io::Error| {
            anyhow!(
                "Can't save settings to {}: {error}. {}",
                path.display(),
                crate::paths::remedy(&error)
            )
        };
        let contents = match fs::read(path) {
            Ok(contents) => {
                let (_, mut file) = parse(&contents, path)?;
                patch(&mut file, held, draft, validated)
                    .with_context(|| format!("Can't save settings to {}", path.display()))?;
                serde_json::to_vec_pretty(&file)?
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                serde_json::to_vec_pretty(&**validated)?
            },
            Err(error) => return Err(cannot_save(error)),
        };
        replace(path, &contents).map_err(cannot_save)
    }
}

/// How far a settings change reaches, ordered by disruption.
///
/// Variant order is load-bearing: `change_to` takes the highest tier, and each tier's action
/// covers the tiers below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Change {
    Unchanged,
    /// Read only by the shell, pill, and tray, so a session continues.
    Presentation,
    /// Read by the owner per session or request, so a session in progress ends.
    Session,
    /// Read when the engine process starts, so the worker reloads.
    Engine,
    /// Desktop options bind when the owners start, so the owners restart.
    Desktop,
}

pub(crate) enum Startup {
    Unconfigured,
    Configured(Box<ValidConfig>),
    Invalid(String),
}

#[derive(Clone)]
pub(crate) struct ValidConfig(Config);

impl ValidConfig {
    pub(crate) fn into_inner(self) -> Config {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn assume(config: Config) -> Self {
        Self(config)
    }
}

impl Deref for ValidConfig {
    type Target = Config;

    fn deref(&self) -> &Config {
        &self.0
    }
}

fn parse(contents: &[u8], path: &Path) -> anyhow::Result<(Config, Map<String, Value>)> {
    let invalid = || format!("Invalid settings in {}", path.display());
    let config: Config = serde_json::from_slice(contents).with_context(invalid)?;
    presets::validate(&config.presets).with_context(invalid)?;
    let file: Map<String, Value> = serde_json::from_slice(contents).with_context(invalid)?;
    Ok((config, file))
}

fn managed_model(engine: Engine, model: &Path, marker: Option<Model>) -> Option<Model> {
    marker.filter(|&choice| engine == Engine::Nemo && Model::at(model) == Some(choice))
}

fn cannot_open(file: &Path, error: &io::Error) -> anyhow::Error {
    if error.kind() == ErrorKind::NotFound {
        anyhow!(
            "File not found: {}. Choose it again in Settings.",
            file.display()
        )
    } else {
        anyhow!(
            "Can't open {} ({error}). Choose it again in Settings.",
            file.display()
        )
    }
}

/// Compares `linux` keys one by one: a default `linux` block is not serialized at all.
fn patch(
    file: &mut Map<String, Value>,
    held: &[&Config],
    draft: &Config,
    validated: &Config,
) -> anyhow::Result<()> {
    for (key, value) in changes(held.iter().copied(), draft, validated)? {
        if key != "linux" {
            apply(file, key, value);
        }
    }
    let linux = changes(
        held.iter().map(|config| &config.linux),
        &draft.linux,
        &validated.linux,
    )?;
    if !linux.is_empty() {
        let section = file
            .entry("linux")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .context("The linux settings must be an object")?;
        for (key, value) in linux {
            apply(section, key, value);
        }
    }
    Ok(())
}

fn apply(section: &mut Map<String, Value>, key: String, value: Option<Value>) {
    match value {
        Some(value) => {
            section.insert(key, value);
        },
        None => {
            section.remove(&key);
        },
    }
}

/// The keys whose draft differs from every held spelling, each with its validated value; `None`
/// removes a key the settings no longer write, such as shortcuts emptied to their default.
fn changes<'a, T: Serialize + 'a>(
    held: impl IntoIterator<Item = &'a T>,
    draft: &T,
    validated: &T,
) -> serde_json::Result<Vec<(String, Option<Value>)>> {
    let held = held
        .into_iter()
        .map(serde_json::to_value)
        .collect::<serde_json::Result<Vec<_>>>()?;
    let draft = serde_json::to_value(draft)?;
    let Value::Object(mut validated) = serde_json::to_value(validated)? else {
        return Err(serde::ser::Error::custom(
            "Settings must serialize as an object",
        ));
    };
    let mut keys: Vec<String> = validated.keys().cloned().collect();
    for key in held.iter().filter_map(Value::as_object).flat_map(Map::keys) {
        if !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    Ok(keys
        .into_iter()
        .filter(|key| held.iter().all(|held| held.get(key) != draft.get(key)))
        .map(|key| {
            let value = validated.remove(&key);
            (key, value)
        })
        .collect())
}

/// Renames a temporary file beside `target` over it, on the same filesystem, so a failure never
/// leaves half a file.
fn replace(target: &Path, contents: &[u8]) -> io::Result<()> {
    let directory = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(contents)?;
    file.persist(target)?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn valid_config(directory: &Path) -> anyhow::Result<Config> {
    let engine_executable = directory.join("engine");
    let model = directory.join("model.gguf");
    fs::write(&engine_executable, b"fake engine")?;
    fs::write(&model, b"fake model")?;
    Ok(Config {
        engine_executable,
        model,
        ..Config::default()
    })
}

#[cfg(test)]
mod tests;
