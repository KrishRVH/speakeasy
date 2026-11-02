use std::path::PathBuf;

use crate::{
    config::{Config, Engine},
    models::{self, Model},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Speech {
    Managed(Model),
    Own,
}

impl Speech {
    pub(super) fn of(config: &Config, own_engine: bool) -> Option<Self> {
        if own_engine {
            return Some(Self::Own);
        }
        config.model_choice().map(Self::Managed).or_else(|| {
            (!config.needs_engine() || !config.model.as_os_str().is_empty()).then_some(Self::Own)
        })
    }
}

pub(super) fn use_own_engine(config: &mut Config) {
    if config.model_choice().is_some() {
        config.nemo_model = None;
        config.model = PathBuf::new();
    }
}

pub(super) fn switch_engine(config: &mut Config, engine: Engine) -> bool {
    if config.engine == engine {
        return false;
    }
    config.engine = engine;
    config.engine_executable = PathBuf::new();
    config.model = PathBuf::new();
    config.nemo_model = None;
    if engine == Engine::Whisper {
        config.language = models::whisper_language(&config.language).into();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parakeet() -> Config {
        Config {
            engine: Engine::Nemo,
            engine_executable: "engines/nemo-speech-cuda/nemo-speech.exe".into(),
            model: format!("models/sha/{}", Model::Parakeet.file_name()).into(),
            nemo_model: Some(Model::Parakeet),
            language: "de-DE".into(),
            ..Config::default()
        }
    }

    #[test]
    fn a_managed_model_is_chosen_only_while_its_file_is_configured() {
        let mut config = parakeet();
        assert_eq!(
            Speech::of(&config, false),
            Some(Speech::Managed(Model::Parakeet))
        );
        assert_eq!(Speech::of(&Config::default(), false), None);
        assert_eq!(Speech::of(&Config::default(), true), Some(Speech::Own));
        use_own_engine(&mut config);
        assert_eq!(Speech::of(&config, true), Some(Speech::Own));
        assert_eq!(
            config.engine_executable,
            parakeet().engine_executable,
            "Choosing your own NeMo model dropped the installed nemo-speech program"
        );
        assert!(config.model.as_os_str().is_empty() && config.nemo_model.is_none());
    }

    #[test]
    fn your_own_engine_stays_chosen_when_its_model_is_a_managed_file() {
        assert_eq!(
            Speech::of(&parakeet(), true),
            Some(Speech::Own),
            "Picking the managed Parakeet file hid the own-engine program picker"
        );
    }

    #[test]
    fn switching_engines_forgets_the_other_engines_files() {
        let mut config = parakeet();
        assert!(switch_engine(&mut config, Engine::Whisper));
        assert!(
            config.engine_executable.as_os_str().is_empty() && config.model.as_os_str().is_empty(),
            "Whisper kept nemo-speech files"
        );
        assert_eq!(config.nemo_model, None);
        assert_eq!(config.language, "de");
        config.engine_executable = "whisper-server.exe".into();
        assert!(!switch_engine(&mut config, Engine::Whisper));
        assert_eq!(
            config.engine_executable,
            std::path::Path::new("whisper-server.exe"),
            "Choosing the current engine cleared its files"
        );
    }
}
