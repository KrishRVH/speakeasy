use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};

use super::{Config, Engine, managed_model};
use crate::{
    models::{self, Model},
    recognition_terms::Terms,
    transcript::vocabulary::Correction,
};

pub(crate) const LIMIT: usize = 16;
const NAME_LIMIT: usize = 48;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

impl Preset {
    pub(crate) fn capture(name: &str, config: &Config) -> anyhow::Result<Self> {
        let name = name.trim();
        validate_name(name)?;
        ensure!(
            config.speech_configured(),
            "Choose the engine executable and a matching model before saving a preset."
        );
        config.validate_choices()?;
        Ok(Self::take(name, config))
    }

    fn take(name: &str, config: &Config) -> Self {
        Self {
            name: name.to_owned(),
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
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn load(&self, config: &mut Config) {
        let Self {
            name: _,
            engine,
            engine_executable,
            model,
            nemo_model,
            language,
            recognition_terms,
            remove_fillers,
            vocabulary,
            threads,
            use_gpu,
        } = self;
        config.engine = *engine;
        config.engine_executable.clone_from(engine_executable);
        config.model.clone_from(model);
        config.nemo_model = *nemo_model;
        config.language.clone_from(language);
        config.recognition_terms.clone_from(recognition_terms);
        config.remove_fillers = *remove_fillers;
        config.vocabulary.clone_from(vocabulary);
        config.threads = *threads;
        config.use_gpu = *use_gpu;
    }

    pub(crate) fn matches(&self, config: &Config) -> bool {
        let Self {
            name: _,
            engine,
            engine_executable,
            model,
            nemo_model,
            language,
            recognition_terms,
            remove_fillers,
            vocabulary,
            threads,
            use_gpu,
        } = self;
        *engine == config.engine
            && *engine_executable == config.engine_executable
            && *model == config.model
            && *nemo_model == config.nemo_model
            && *language == config.language
            && *recognition_terms == config.recognition_terms
            && *remove_fillers == config.remove_fillers
            && *vocabulary == config.vocabulary
            && *threads == config.threads
            && *use_gpu == config.use_gpu
    }

    pub(crate) fn description(&self) -> String {
        let managed = managed_model(self.engine, &self.model, self.nemo_model);
        let model = match managed {
            Some(model) => model.label(),
            None => self
                .model
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("No model chosen"),
        };
        let languages = if self.engine == Engine::Whisper {
            models::WHISPER_LANGUAGES
        } else {
            models::LANGUAGES
        };
        let language = (self.language != "auto"
            && (self.engine == Engine::Whisper || managed == Some(Model::Nemotron)))
        .then(|| {
            languages
                .iter()
                .find_map(|&(code, name)| (code == self.language).then_some(name))
                .map_or_else(|| self.language.clone(), str::to_owned)
        });
        let count = |amount: usize, noun: &str| match amount {
            0 => None,
            1 => Some(format!("1 {noun}")),
            _ => Some(format!("{amount} {noun}s")),
        };
        [
            Some(model.to_owned()),
            language,
            count(self.vocabulary.len(), "correction"),
            count(self.recognition_terms.phrases().len(), "hint"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

pub(crate) fn validate(presets: &[Preset]) -> anyhow::Result<()> {
    ensure!(
        presets.len() <= LIMIT,
        "Keep at most {LIMIT} dictation presets."
    );
    let mut names = HashSet::new();
    for (row, preset) in (1..).zip(presets) {
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
