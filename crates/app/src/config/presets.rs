//! Named dictation choices load into the existing draft; they never own a session or global options.

use std::{collections::HashSet, fmt, path::PathBuf};

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};

use super::{Config, Engine};
use crate::{models::Model, recognition_terms::Terms, transcript::vocabulary::Correction};

pub(crate) const LIMIT: usize = 16;
const NAME_LIMIT: usize = 48;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preset {
    name: String,
    engine: Engine,
    engine_executable: PathBuf,
    model: PathBuf,
    nemo_model: Option<Model>,
    language: String,
    recognition_terms: Terms,
    remove_fillers: bool,
    vocabulary: Vec<Correction>,
    threads: u16,
    use_gpu: bool,
}

impl fmt::Debug for Preset {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Preset").finish_non_exhaustive()
    }
}

impl Preset {
    pub(crate) fn capture(name: String, config: &Config) -> anyhow::Result<Self> {
        let name = name.trim().to_owned();
        validate_name(&name)?;
        config.validate_choices()?;
        Ok(Self {
            name,
            engine: config.engine,
            engine_executable: config.engine_executable.clone(),
            model: config.model.clone(),
            nemo_model: config.nemo_model,
            language: config.language.clone(),
            recognition_terms: config.recognition_terms.clone(),
            remove_fillers: config.remove_fillers,
            vocabulary: config.vocabulary.clone(),
            threads: config.threads,
            use_gpu: config.use_gpu,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn load(&self, config: &mut Config) {
        config.engine = self.engine;
        config.engine_executable.clone_from(&self.engine_executable);
        config.model.clone_from(&self.model);
        config.nemo_model = self.nemo_model;
        config.language.clone_from(&self.language);
        config.recognition_terms.clone_from(&self.recognition_terms);
        config.remove_fillers = self.remove_fillers;
        config.vocabulary.clone_from(&self.vocabulary);
        config.threads = self.threads;
        config.use_gpu = self.use_gpu;
    }

    pub(crate) fn description(&self) -> String {
        let model = self
            .model
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Choose a model");
        format!(
            "{} · {model} · {} · {} corrections",
            self.engine.label(),
            self.language,
            self.vocabulary.len()
        )
    }
}

pub(crate) fn validate(presets: &[Preset]) -> anyhow::Result<()> {
    ensure!(
        presets.len() <= LIMIT,
        "Keep at most {LIMIT} dictation presets."
    );
    let mut names = HashSet::new();
    for (index, preset) in presets.iter().enumerate() {
        let row = index.saturating_add(1);
        validate_name(&preset.name).with_context(|| format!("Preset {row}"))?;
        ensure!(
            names.insert(preset.name.to_lowercase()),
            "Preset {row} repeats an earlier name. Choose distinct names."
        );
        let mut choices = Config::default();
        preset.load(&mut choices);
        choices
            .validate_choices()
            .with_context(|| format!("Preset {row}"))?;
    }
    Ok(())
}

fn validate_name(name: &str) -> anyhow::Result<()> {
    ensure!(
        !name.is_empty()
            && name == name.trim()
            && name.chars().count() <= NAME_LIMIT
            && !name.chars().any(char::is_control),
        "Use a preset name of 1 to {NAME_LIMIT} characters, without surrounding spaces or control characters."
    );
    Ok(())
}

#[cfg(test)]
mod tests;
