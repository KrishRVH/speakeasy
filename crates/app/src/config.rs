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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
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
        if self.engine == Engine::Parakeet && !self.use_gpu {
            bail!("Parakeet requires GPU acceleration. Enable Prefer GPU or select Whisper.");
        }
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
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("speakeasy/settings.json")
}
