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
use serde_path_to_error::Segment;
use speakeasy_platform::{Delivery, DesktopOptions, LinuxPaste, MacShortcut};

use crate::{
    models::{self, Model},
    recognition_terms::Terms,
    theme::Theme,
    transcript::{
        Fillers,
        vocabulary::{self, Correction},
    },
};

const LINK_DEPTH: usize = 40;

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
    reason = "Each flag persists an independent user preference that can be toggled separately"
)]
pub(crate) struct Config {
    #[serde(skip_serializing_if = "LinuxSettings::is_default")]
    pub linux: LinuxSettings,
    pub mac_shortcut: MacShortcut,
    pub engine: Engine,
    pub microphone: Option<String>,
    pub engine_executable: PathBuf,
    pub model: PathBuf,
    pub nemo_model: Option<Model>,
    pub language: String,
    pub recognition_terms: Terms,
    pub remove_fillers: bool,
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
            engine: Engine::Nemo,
            microphone: None,
            engine_executable: PathBuf::new(),
            model: PathBuf::new(),
            nemo_model: None,
            language: "auto".into(),
            recognition_terms: Terms::default(),
            remove_fillers: true,
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
        if self.remove_fillers
            && language
                .split('-')
                .next()
                .is_some_and(|code| code.eq_ignore_ascii_case("en"))
        {
            Fillers::Remove
        } else {
            Fillers::Preserve
        }
    }

    pub(crate) fn uses_nemotron(&self) -> bool {
        self.model_choice() == Some(Model::Nemotron)
    }

    pub(crate) fn model_choice(&self) -> Option<Model> {
        self.nemo_model
            .filter(|choice| self.engine == Engine::Nemo && Model::at(&self.model) == Some(*choice))
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

    pub(crate) fn desktop_options(&self) -> DesktopOptions {
        DesktopOptions {
            mac_shortcut: self.mac_shortcut,
            shortcut: self.linux.shortcut.clone(),
            cancel: self.linux.cancel.clone(),
            paste: self.text_delivery.linux_paste(),
            external_shortcut: self.linux.external_shortcut,
        }
    }

    pub(crate) fn read(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
        Ok(parse(&contents, path)?.0)
    }

    pub(crate) fn is_unreadable(path: &Path) -> bool {
        path.exists() && Self::read(path).is_err()
    }

    pub(crate) fn discard(path: &Path) -> io::Result<()> {
        match fs::remove_file(link_target(path)?) {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            removed => removed,
        }
    }

    pub(crate) fn load(path: &Path) -> (Self, Startup) {
        if !path.exists() {
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
        if path.exists() {
            draft.clone()
        } else {
            Self::default()
        }
    }

    pub(crate) fn needs_engine(&self) -> bool {
        self.engine_executable.as_os_str().is_empty()
    }

    pub(crate) fn change_to(&self, next: &Self) -> Change {
        let Self {
            linux,
            mac_shortcut,
            engine,
            microphone,
            engine_executable,
            model,
            nemo_model,
            language,
            recognition_terms,
            remove_fillers,
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
                    || (cfg!(target_os = "macos") && self.mac_shortcut != *mac_shortcut),
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
                self.language != *language
                    || self.microphone != *microphone
                    || self.recognition_terms != *recognition_terms
                    || self.remove_fillers != *remove_fillers
                    || self.vocabulary != *vocabulary
                    || self.text_delivery != *text_delivery
                    || self.play_sounds != *play_sounds,
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
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let engine = &config.engine_executable;
            let metadata = engine
                .metadata()
                .map_err(|error| cannot_open(engine, &error))?;
            ensure!(
                metadata.permissions().mode() & 0o111 != 0,
                "Speech executable is not executable. Choose an installed engine or enable its executable permission."
            );
        }
        Ok(ValidConfig(config))
    }

    fn validate_choices(&self) -> anyhow::Result<()> {
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

    pub(crate) fn save_changes(
        held: &[&Self],
        draft: &Self,
        validated: &ValidConfig,
        path: &Path,
    ) -> anyhow::Result<()> {
        let cannot_save = |error: io::Error| {
            anyhow!(
                "Cannot save settings to {}: {error}. {}",
                path.display(),
                crate::paths::remedy(&error)
            )
        };
        let target = link_target(path).map_err(cannot_save)?;
        let contents = match fs::read(&target) {
            Ok(contents) => {
                let (_, mut file) = parse(&contents, path)?;
                patch(&mut file, held, draft, validated)
                    .with_context(|| format!("Cannot save settings to {}", path.display()))?;
                serde_json::to_vec_pretty(&file)?
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                serde_json::to_vec_pretty(&**validated)?
            },
            Err(error) => return Err(cannot_save(error)),
        };
        replace(&target, &contents).map_err(cannot_save)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Change {
    Unchanged,
    Presentation,
    Session,
    Engine,
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
    let config: Config = from_json(contents).with_context(invalid)?;
    presets::validate(&config.presets).with_context(invalid)?;
    let file: Map<String, Value> = from_json(contents).with_context(invalid)?;
    if file.get("linux").is_some_and(|linux| !linux.is_object()) {
        return Err(anyhow!("The linux settings must be an object").context(invalid()));
    }
    Ok((config, file))
}

fn from_json<'de, T: Deserialize<'de>>(contents: &'de [u8]) -> anyhow::Result<T> {
    let mut json = serde_json::Deserializer::from_slice(contents);
    let value = serde_path_to_error::deserialize(&mut json).map_err(|error| {
        if let Some(Segment::Map { key }) = error.path().iter().next()
            && matches!(key.as_str(), "vocabulary" | "recognition_terms" | "presets")
        {
            return anyhow!(
                "{key}: invalid private setting at line {} column {}",
                error.inner().line(),
                error.inner().column()
            );
        }
        let key = error
            .path()
            .iter()
            .map_while(|segment| match segment {
                Segment::Map { key: name } => Some(name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(".");
        let account = anyhow::Error::new(error.into_inner());
        if key.is_empty() {
            account
        } else {
            account.context(key)
        }
    })?;
    json.end()?;
    Ok(value)
}

fn link_target(path: &Path) -> io::Result<PathBuf> {
    let mut target = path.to_owned();
    for _ in 0..LINK_DEPTH {
        if !target.is_symlink() {
            return Ok(target);
        }
        let link = fs::read_link(&target)?;
        target = match target.parent() {
            Some(folder) => folder.join(link),
            None => link,
        };
    }
    Err(io::Error::other("Too many levels of symbolic links"))
}

fn cannot_open(file: &Path, error: &io::Error) -> anyhow::Error {
    if error.kind() == ErrorKind::NotFound {
        anyhow!(
            "File not found: {}. Choose it again in Settings.",
            file.display()
        )
    } else {
        anyhow!(
            "Cannot open {} ({error}). Choose it again in Settings.",
            file.display()
        )
    }
}

fn patch(
    file: &mut Map<String, Value>,
    held: &[&Config],
    draft: &Config,
    validated: &Config,
) -> anyhow::Result<()> {
    for (key, value) in changes(held.iter().copied(), draft, validated)? {
        if key != "linux" {
            file.insert(key, value);
        }
    }
    let linux = changes(
        held.iter().map(|config| &config.linux),
        &draft.linux,
        &validated.linux,
    )?;
    if !linux.is_empty() {
        file.entry("linux")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .context("parse admits only a linux object")?
            .extend(linux);
    }
    Ok(())
}

fn changes<'a, T: Serialize + 'a>(
    held: impl IntoIterator<Item = &'a T>,
    draft: &T,
    validated: &T,
) -> serde_json::Result<Vec<(String, Value)>> {
    let held = held
        .into_iter()
        .map(serde_json::to_value)
        .collect::<serde_json::Result<Vec<_>>>()?;
    let draft = serde_json::to_value(draft)?;
    let Value::Object(validated) = serde_json::to_value(validated)? else {
        return Ok(Vec::new());
    };
    Ok(validated
        .into_iter()
        .filter(|(key, _)| held.iter().all(|held| held.get(key) != draft.get(key)))
        .collect())
}

fn replace(target: &Path, contents: &[u8]) -> io::Result<()> {
    let directory = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    file.persist(target)?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn valid_config(directory: &Path) -> anyhow::Result<Config> {
    let engine_executable = directory.join("engine");
    let model = directory.join("model.gguf");
    fs::write(&engine_executable, b"fake engine")?;
    fs::write(&model, b"fake model")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&engine_executable, fs::Permissions::from_mode(0o700))?;
    }
    Ok(Config {
        engine_executable,
        model,
        ..Config::default()
    })
}

#[cfg(test)]
mod tests;
