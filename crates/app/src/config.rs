use crate::theme::Theme;
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    #[default]
    Whisper,
    Parakeet,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "LinuxSettings::is_default")]
    pub linux: LinuxSettings,
    #[serde(default)]
    pub engine: Engine,
    #[serde(default)]
    pub microphone: Option<String>,
    pub engine_executable: PathBuf,
    pub model: PathBuf,
    #[serde(default = "language")]
    pub language: String,
    #[serde(default = "threads")]
    pub threads: u16,
    #[serde(default = "yes")]
    pub use_gpu: bool,
    #[serde(default)]
    pub reduced_motion: bool,
    #[serde(default)]
    pub preserve_clipboard: bool,
    #[serde(default)]
    pub theme: Theme,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LinuxSettings {
    pub shortcut: String,
    pub cancel: String,
    pub terminal_paste: bool,
    pub manual_paste: bool,
    pub external_shortcut: bool,
}
impl Default for LinuxSettings {
    fn default() -> Self {
        let options = speakeasy_platform::DesktopOptions::default();
        Self {
            shortcut: options.shortcut,
            cancel: options.cancel,
            terminal_paste: false,
            manual_paste: false,
            external_shortcut: false,
        }
    }
}
impl LinuxSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
fn language() -> String {
    "en".into()
}
fn threads() -> u16 {
    4
}
fn yes() -> bool {
    true
}

impl Config {
    pub fn desktop_options(&self) -> speakeasy_platform::DesktopOptions {
        speakeasy_platform::DesktopOptions {
            shortcut: self.linux.shortcut.clone(),
            cancel: self.linux.cancel.clone(),
            terminal_paste: self.linux.terminal_paste,
            manual_paste: self.linux.manual_paste,
            external_shortcut: self.linux.external_shortcut,
        }
    }
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        serde_json::from_slice(
            &std::fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?,
        )
        .with_context(|| {
            format!(
                "Invalid settings in {}. Repair this file before saving changes.",
                path.display()
            )
        })
    }

    #[cfg(test)]
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut config = Self::read(path)?;
        config.validate(path)?;
        Ok(config)
    }

    pub fn speech_changed(&self, next: &Self) -> bool {
        self.engine != next.engine
            || self.engine_executable != next.engine_executable
            || self.model != next.model
            || self.threads != next.threads
            || self.use_gpu != next.use_gpu
    }

    pub fn validate(&mut self, path: &Path) -> anyhow::Result<()> {
        let directory = path.parent().unwrap_or(Path::new("."));
        for file in [&mut self.engine_executable, &mut self.model] {
            if file.as_os_str().is_empty() {
                bail!("Choose the local speech executable and a matching model.");
            }
            if file.is_relative() {
                *file = directory.join(&file);
            }
            *file = file
                .canonicalize()
                .with_context(|| format!("File not found: {}", file.display()))?;
            if !file.is_file() {
                bail!("Expected a file: {}", file.display());
            }
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            if self.engine_executable.metadata()?.permissions().mode() & 0o111 == 0 {
                bail!(
                    "Speech executable is not executable. Choose an installed engine or enable its executable permission."
                );
            }
        }
        if self.threads == 0 || self.threads > 256 {
            bail!("threads must be between 1 and 256");
        }
        if self.language.is_empty() || self.language.len() > 16 {
            bail!("Set language to a language code or auto");
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let directory = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory)?;
        if path.exists() {
            Self::read(path)?;
        }
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        use std::io::Write;
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
            engine_executable: PathBuf::new(),
            model: PathBuf::new(),
            language: language(),
            threads: threads(),
            use_gpu: true,
            reduced_motion: false,
            preserve_clipboard: false,
            theme: Theme::default(),
            microphone: None,
        }
    }
}

pub fn default_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("speakeasy/settings.json")
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
        let defaults: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
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
        assert!(restored.linux == config.linux);
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
        std::fs::write(&executable, b"fixture")?;
        std::fs::write(&model, b"fixture")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600))?;
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
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))?;
        config.validate(&path)?;
        Ok(())
    }
}
