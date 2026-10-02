//! Typed settings: path resolution, validation, and atomic saves. The JSON shape is a user-edited
//! contract, so unknown fields are rejected and a default Linux block stays out of saved files.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use speakeasy_platform::{Delivery, DesktopOptions};

use crate::theme::Theme;

/// The local speech engine family the configured executable belongs to.
#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// A whisper.cpp server.
    #[default]
    Whisper,
    /// A NeMo-Speech.cpp server running Parakeet.
    Parakeet,
}

impl Engine {
    /// The name Settings and the tray show.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Whisper => "Whisper",
            Self::Parakeet => "Parakeet",
        }
    }
}

/// The user's settings file.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each flag persists an independent user preference that can be toggled separately"
)]
pub struct Config {
    /// Linux desktop bindings, omitted from saved files while they hold defaults.
    #[serde(default, skip_serializing_if = "LinuxSettings::is_default")]
    pub linux: LinuxSettings,
    /// The engine family `engine_executable` belongs to.
    #[serde(default)]
    pub engine: Engine,
    /// The saved microphone identifier; `None` follows the system default.
    #[serde(default)]
    pub microphone: Option<String>,
    /// The local speech server executable.
    pub engine_executable: PathBuf,
    /// The model the engine loads.
    pub model: PathBuf,
    /// Whisper's language code, or `auto`.
    #[serde(default = "default_language")]
    pub language: String,
    /// Whether English hesitations (`um`, `uh`) are removed before insertion.
    #[serde(default = "default_enabled")]
    pub remove_fillers: bool,
    /// Whisper's CPU thread count.
    #[serde(default = "default_threads")]
    pub threads: u16,
    /// Whether the engine may use the GPU.
    #[serde(default = "default_enabled")]
    pub use_gpu: bool,
    /// Whether motion is reduced regardless of the OS preference.
    #[serde(default)]
    pub reduced_motion: bool,
    /// Whether text is typed directly, leaving the clipboard untouched.
    #[serde(default)]
    pub preserve_clipboard: bool,
    /// The color theme.
    #[serde(default)]
    pub theme: Theme,
}

impl Config {
    /// How dictated text reaches the focused application.
    #[must_use]
    pub const fn delivery(&self) -> Delivery {
        if self.preserve_clipboard {
            Delivery::Direct
        } else {
            Delivery::Paste
        }
    }

    /// Whether the user or the OS asks for reduced motion.
    #[must_use]
    pub fn prefers_reduced_motion(&self) -> bool {
        self.reduced_motion || speakeasy_platform::reduced_motion()
    }

    /// The Linux desktop bindings native input uses.
    #[must_use]
    pub fn desktop_options(&self) -> DesktopOptions {
        DesktopOptions {
            shortcut: self.linux.shortcut.clone(),
            cancel: self.linux.cancel.clone(),
            terminal_paste: self.linux.terminal_paste,
            manual_paste: self.linux.manual_paste,
            external_shortcut: self.linux.external_shortcut,
        }
    }

    /// Reads settings without validating their paths.
    ///
    /// # Errors
    /// Returns an error naming the file when it is unreadable or invalid.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        serde_json::from_slice(
            &fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?,
        )
        .with_context(|| {
            format!(
                "Invalid settings in {}. Repair this file before saving changes.",
                path.display()
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        let mut config = Self::read(path)?;
        config.validate(path)?;
        Ok(config)
    }

    /// Whether `next` needs a different engine process.
    #[must_use]
    pub fn speech_changed(&self, next: &Self) -> bool {
        self.engine != next.engine
            || self.engine_executable != next.engine_executable
            || self.model != next.model
            || self.threads != next.threads
            || self.use_gpu != next.use_gpu
    }

    /// Resolves the engine and model against the settings directory and canonicalizes them, then
    /// checks that they exist and that the remaining values are in range.
    ///
    /// # Errors
    /// Returns an actionable error for a missing file or an out-of-range value.
    pub fn validate(&mut self, path: &Path) -> anyhow::Result<()> {
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

    /// Writes the settings atomically, refusing to replace a file the user still has to repair.
    ///
    /// # Errors
    /// Returns an error if the existing file is invalid or the write fails.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
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
            language: default_language(),
            remove_fillers: default_enabled(),
            threads: default_threads(),
            use_gpu: default_enabled(),
            reduced_motion: false,
            preserve_clipboard: false,
            theme: Theme::default(),
        }
    }
}

/// Linux desktop bindings.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LinuxSettings {
    /// The dictation shortcut.
    pub shortcut: String,
    /// The cancellation shortcut.
    pub cancel: String,
    /// Whether paste uses the terminal shortcut.
    pub terminal_paste: bool,
    /// Whether text is only copied.
    pub manual_paste: bool,
    /// Whether desktop command bindings replace the shortcut.
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

/// The settings file location when `--config` is absent.
#[must_use]
pub fn default_path() -> PathBuf {
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

fn default_language() -> String {
    "en".into()
}

fn default_threads() -> u16 {
    4
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_settings_preserve_linux_bindings_and_skip_defaults() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let mut config = Config::default();
        config.save(&path)?;
        let defaults: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert!(defaults.get("linux").is_none());
        assert!(Config::read(&path)?.linux.is_default());

        config.linux = LinuxSettings {
            shortcut: "CTRL+ALT+d".into(),
            cancel: "CTRL+ALT+Escape".into(),
            terminal_paste: true,
            manual_paste: true,
            external_shortcut: true,
        };
        config.save(&path)?;
        let restored = Config::read(&path)?;
        assert_eq!(restored.linux, config.linux);
        let options = restored.desktop_options();
        assert_eq!(options.shortcut, "CTRL+ALT+d");
        assert_eq!(options.cancel, "CTRL+ALT+Escape");
        assert!(options.terminal_paste && options.manual_paste && options.external_shortcut);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn validation_requires_an_executable_engine() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("engine");
        let model = directory.path().join("model.gguf");
        fs::write(&executable, b"fixture")?;
        fs::write(&model, b"fixture")?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o600))?;
        let mut config = Config {
            engine_executable: executable.clone(),
            model,
            ..Config::default()
        };
        let path = directory.path().join("settings.json");
        let failure = config
            .validate(&path)
            .err()
            .context("An engine without executable permission was accepted")?;
        assert!(failure.to_string().contains("not executable"));
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700))?;
        config.validate(&path)?;
        Ok(())
    }
}
