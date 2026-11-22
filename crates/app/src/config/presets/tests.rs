use super::*;
use crate::{
    config::{Change, Startup, TextDelivery, ValidConfig, valid_config},
    theme::Theme,
    transcript::vocabulary::Vocabulary,
};

fn synthetic() -> Config {
    Config {
        engine_executable: "synthetic-engine".into(),
        model: "synthetic-model".into(),
        ..Config::default()
    }
}

#[test]
fn loading_a_domain_restores_its_words_and_model_without_changing_global_preferences()
-> anyhow::Result<()> {
    let mut writing = Config {
        engine: Engine::Whisper,
        microphone: Some("Chosen microphone".into()),
        text_delivery: TextDelivery::Type,
        theme: Theme::Iris,
        play_sounds: false,
        ..synthetic()
    };
    let prose = Preset::capture("Writing", &writing)?;
    let mut recognition_terms = Terms::default();
    recognition_terms.add("Kubernetes")?;
    let coding = Config {
        engine: Engine::Nemo,
        engine_executable: "nemo-speech.exe".into(),
        model: Model::Nemotron.file_name().into(),
        nemo_model: Some(Model::Nemotron),
        language: "en-US".into(),
        recognition_terms,
        vocabulary: vec![Correction {
            heard: "rust".into(),
            written: "Rust".into(),
        }],
        threads: 8,
        use_gpu: false,
        remove_fillers: false,
        ..Config::default()
    };
    let technical = Preset::capture("Coding", &coding)?;
    technical.load(&mut writing);
    assert_eq!(
        Vocabulary::new(&writing.vocabulary).text("Use rust.".into()),
        "Use Rust."
    );
    assert_eq!(writing.engine, Engine::Nemo);
    assert_eq!(writing.language, "en-US");
    assert_eq!(writing.recognition_terms, technical.recognition_terms);
    assert_eq!(writing.microphone.as_deref(), Some("Chosen microphone"));
    assert!(writing.text_delivery == TextDelivery::Type && !writing.play_sounds);
    assert_eq!(writing.theme, Theme::Iris);
    prose.load(&mut writing);
    assert_eq!(
        Vocabulary::new(&writing.vocabulary).text("Remove the rust.".into()),
        "Remove the rust."
    );
    assert_eq!(writing.engine, Engine::Whisper);
    assert_eq!(writing.recognition_terms.phrases(), Vec::<String>::new());
    Ok(())
}

#[test]
fn inactive_paths_survive_save_and_only_a_loaded_missing_model_prevents_application()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let held = valid_config(directory.path())?;
    std::fs::write(&path, serde_json::to_vec(&held)?)?;
    let missing = Config {
        engine_executable: "old/engine".into(),
        model: "old/model.gguf".into(),
        ..held.clone()
    };
    let mut draft = held.clone();
    draft
        .presets
        .push(Preset::capture("Earlier model", &missing)?);
    assert_eq!(held.change_to(&draft), Change::Presentation);
    let mut file: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    file["play_sounds"] = false.into();
    std::fs::write(&path, serde_json::to_vec(&file)?)?;
    Config::save_changes(&[&held], &draft, &draft.validated(&path)?, &path)?;
    let saved = Config::read(&path)?;
    assert!(!saved.play_sounds);
    assert_eq!(saved.presets[0].model, PathBuf::from("old/model.gguf"));
    assert!(saved.validated(&path).is_ok());
    let mut loaded = saved.clone();
    saved.presets[0].load(&mut loaded);
    assert_eq!(saved.change_to(&loaded), Change::Engine);
    assert!(loaded.validated(&path).is_err());
    Ok(())
}

#[test]
fn settings_with_a_preset_saved_without_a_model_still_load() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let held = valid_config(directory.path())?;
    let mut incomplete = serde_json::to_value(Preset::capture("Earlier", &held)?)?;
    incomplete["model"] = "".into();
    let mut file = serde_json::to_value(&held)?;
    file["presets"] = serde_json::json!([incomplete]);
    std::fs::write(&path, serde_json::to_vec(&file)?)?;
    let (loaded, Startup::Configured(valid)) = Config::load(&path) else {
        anyhow::bail!("A preset saved without a model made the whole settings file invalid");
    };
    assert_eq!(loaded.engine_executable, valid.engine_executable);
    let mut chosen = loaded;
    let [earlier] = valid.presets.as_slice() else {
        anyhow::bail!("Loading dropped the incomplete preset");
    };
    earlier.load(&mut chosen);
    assert!(!chosen.speech_configured());
    Ok(())
}

#[test]
fn sparse_settings_need_no_presets_and_invalid_libraries_are_rejected_even_without_an_engine()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    std::fs::write(&path, "{}")?;
    assert_eq!(Config::read(&path)?.presets, Vec::<Preset>::new());
    let preset = serde_json::to_value(Preset::capture("PRIVATE_NAME", &synthetic())?)?;
    for invalid in [
        serde_json::json!({"presets": vec![preset.clone(); LIMIT + 1]}),
        serde_json::json!({"presets": [preset, preset]}),
        {
            let mut invalid = preset.clone();
            invalid["threads"] = 0.into();
            serde_json::json!({"presets": [invalid]})
        },
        {
            let mut invalid = preset;
            invalid["name"] = "PRIVATE_NAME\n".into();
            serde_json::json!({"presets": [invalid]})
        },
    ] {
        std::fs::write(&path, serde_json::to_vec(&invalid)?)?;
        let error = Config::read(&path).unwrap_err();
        assert!(!format!("{error:#}").contains("PRIVATE_NAME"));
        assert!(matches!(Config::load(&path).1, Startup::Invalid(_)));
        assert!(
            Config::save_changes(
                &[&Config::default()],
                &Config::default(),
                &ValidConfig::assume(Config::default()),
                &path
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn a_description_names_the_model_a_preset_runs_even_under_a_stale_marker() -> anyhow::Result<()> {
    let managed = Config {
        engine: Engine::Nemo,
        model: Model::Nemotron.file_name().into(),
        nemo_model: Some(Model::Nemotron),
        language: "de-DE".into(),
        ..synthetic()
    };
    let parakeet_file = Config {
        model: Model::Parakeet.file_name().into(),
        ..managed.clone()
    };
    let whisper_file = Config {
        engine: Engine::Whisper,
        model: "ggml-base.bin".into(),
        language: "de".into(),
        ..managed.clone()
    };
    for (config, description) in [
        (managed, "Nemotron 3.5 · German"),
        (parakeet_file, Model::Parakeet.file_name()),
        (whisper_file, "ggml-base.bin · German"),
    ] {
        let preset = Preset::capture("Fixture", &config)?;
        assert_eq!(preset.description(), description);
    }
    Ok(())
}

#[test]
fn names_cannot_hide_case_duplicates_or_controls() -> anyhow::Result<()> {
    let config = synthetic();
    let first = Preset::capture("medical", &config)?;
    let upper = Preset::capture("MEDICAL", &config)?;
    assert!(validate(&[first, upper]).is_err());
    assert!(Preset::capture("medical\u{7}notes", &config).is_err());
    Ok(())
}

#[test]
fn a_preset_needs_both_speech_paths() {
    for config in [
        Config {
            model: PathBuf::new(),
            ..synthetic()
        },
        Config {
            engine_executable: PathBuf::new(),
            ..synthetic()
        },
    ] {
        assert!(Preset::capture("Incomplete", &config).is_err());
    }
}
