//! Typed settings: path resolution, validation, and atomic saves.
//!
//! The JSON file is a user-edited contract that outlives releases. Missing keys read as
//! [`Config::default`], so a new field needs no attribute, and unknown top-level keys are ignored,
//! so a file from another release still loads; a save drops them. A renamed key keeps its old name
//! with `#[serde(alias = "old")]`, on the field or on the enum variant that replaces a retired
//! Engine or Theme, and a retyped key goes through a private wire struct that migrates the old
//! shape. [`LinuxSettings`] stays strict because only hand edits reach it, so retiring one of its
//! keys must keep accepting that key. A default Linux block stays out of saved files.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use speakeasy_platform::{Delivery, DesktopOptions};

use crate::theme::Theme;

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Engine {
    #[default]
    Whisper,
    Parakeet,
}

impl Engine {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Whisper => "Whisper",
            Self::Parakeet => "Parakeet",
        }
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each flag persists an independent user preference that can be toggled separately"
)]
pub(crate) struct Config {
    #[serde(skip_serializing_if = "LinuxSettings::is_default")]
    pub linux: LinuxSettings,
    pub engine: Engine,
    pub microphone: Option<String>,
    pub engine_executable: PathBuf,
    pub model: PathBuf,
    pub language: String,
    pub remove_fillers: bool,
    pub threads: u16,
    pub use_gpu: bool,
    pub reduced_motion: bool,
    pub preserve_clipboard: bool,
    pub theme: Theme,
}

impl Config {
    pub(crate) const fn delivery(&self) -> Delivery {
        if self.preserve_clipboard {
            Delivery::Direct
        } else {
            Delivery::Paste
        }
    }

    /// Whether the user or the OS asks for reduced motion.
    pub(crate) fn prefers_reduced_motion(&self) -> bool {
        self.reduced_motion || speakeasy_platform::reduced_motion()
    }

    pub(crate) fn desktop_options(&self) -> DesktopOptions {
        DesktopOptions {
            shortcut: self.linux.shortcut.clone(),
            cancel: self.linux.cancel.clone(),
            terminal_paste: self.linux.terminal_paste,
            manual_paste: self.linux.manual_paste,
            external_shortcut: self.linux.external_shortcut,
        }
    }

    pub(crate) fn read(path: &Path) -> anyhow::Result<Self> {
        serde_json::from_slice(
            &fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?,
        )
        .with_context(|| format!("Invalid settings in {}", path.display()))
    }

    #[cfg(test)]
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        let mut config = Self::read(path)?;
        config.validate(path)?;
        Ok(config)
    }

    /// Whether `next` needs a different engine process.
    pub(crate) fn speech_changed(&self, next: &Self) -> bool {
        self.engine != next.engine
            || self.engine_executable != next.engine_executable
            || self.model != next.model
            || self.threads != next.threads
            || self.use_gpu != next.use_gpu
    }

    /// Resolves the engine and model against the settings directory and canonicalizes them, then
    /// checks that they exist and that the remaining values are in range.
    pub(crate) fn validate(&mut self, path: &Path) -> anyhow::Result<()> {
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        for file in [&mut self.engine_executable, &mut self.model] {
            ensure!(
                !file.as_os_str().is_empty(),
                "Choose the local speech executable and a matching model."
            );
            if file.is_relative() {
                *file = directory.join(&file);
            }
            *file = file
                .canonicalize()
                .with_context(|| format!("File not found: {}", file.display()))?;
            ensure!(file.is_file(), "Expected a file: {}", file.display());
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                self.engine_executable.metadata()?.permissions().mode() & 0o111 != 0,
                "Speech executable is not executable. Choose an installed engine or enable its executable permission."
            );
        }
        ensure!(
            (1..=256).contains(&self.threads),
            "threads must be between 1 and 256"
        );
        ensure!(
            (1..=16).contains(&self.language.len()),
            "Set language to a language code or auto"
        );
        Ok(())
    }

    pub(crate) fn save(&self, path: &Path) -> anyhow::Result<()> {
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory)?;
        // Never replace a file the user still has to repair.
        if path.exists() {
            Self::read(path)?;
        }
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            linux: LinuxSettings::default(),
            engine: Engine::default(),
            microphone: None,
            engine_executable: PathBuf::new(),
            model: PathBuf::new(),
            language: "en".into(),
            remove_fillers: true,
            threads: 4,
            use_gpu: true,
            reduced_motion: false,
            preserve_clipboard: false,
            theme: Theme::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LinuxSettings {
    pub shortcut: String,
    pub cancel: String,
    pub terminal_paste: bool,
    pub manual_paste: bool,
    pub external_shortcut: bool,
}

impl LinuxSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for LinuxSettings {
    fn default() -> Self {
        let options = DesktopOptions::default();
        Self {
            shortcut: options.shortcut,
            cancel: options.cancel,
            terminal_paste: false,
            manual_paste: false,
            external_shortcut: false,
        }
    }
}

pub(crate) fn default_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(target_os = "linux")]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("speakeasy/settings.json")
}

/// Settings that pass validation: a fake engine and model written into `directory`.
#[cfg(test)]
pub(crate) fn valid_config(directory: &Path) -> anyhow::Result<Config> {
    let engine_executable = directory.join("engine");
    let model = directory.join("model.gguf");
    fs::write(&engine_executable, b"fake engine")?;
    fs::write(&model, b"fake model")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
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
