use proptest::prelude::*;

use super::*;
use crate::{
    config::{Change, ValidConfig, valid_config},
    theme::Theme,
    transcript::vocabulary::Vocabulary,
};

#[test]
fn loading_a_domain_restores_its_words_and_model_without_changing_global_preferences()
-> anyhow::Result<()> {
    let mut writing = Config {
        microphone: Some("Chosen microphone".into()),
        preserve_clipboard: true,
        theme: Theme::Iris,
        play_sounds: false,
        ..Config::default()
    };
    let prose = Preset::capture("Writing".into(), &writing)?;
    let mut coding = writing.clone();
    coding.engine = Engine::Parakeet;
    coding.engine_executable = "nemo-speech.exe".into();
    coding.model = Model::Nemotron.file_name().into();
    coding.nemo_model = Some(Model::Nemotron);
    coding.language = "en-US".into();
    coding.recognition_terms.add("Kubernetes")?;
    coding.vocabulary = vec![Correction {
        heard: "rust".into(),
        written: "Rust".into(),
    }];
    coding.threads = 8;
    coding.use_gpu = false;
    coding.remove_fillers = false;
    coding.microphone = None;
    coding.preserve_clipboard = false;
    coding.theme = Theme::default();
    coding.play_sounds = true;
    let technical = Preset::capture("Coding".into(), &coding)?;
    technical.load(&mut writing);
    assert_eq!(
        Vocabulary::new(&writing.vocabulary).text("Use rust."),
        "Use Rust."
    );
    assert_eq!(writing.engine, Engine::Parakeet);
    assert_eq!(writing.language, "en-US");
    assert_eq!(writing.recognition_terms, technical.recognition_terms);
    assert_eq!(writing.microphone.as_deref(), Some("Chosen microphone"));
    assert!(writing.preserve_clipboard && !writing.play_sounds);
    assert_eq!(writing.theme, Theme::Iris);
    prose.load(&mut writing);
    assert_eq!(
        Vocabulary::new(&writing.vocabulary).text("Remove the rust."),
        "Remove the rust."
    );
    assert_eq!(writing.engine, Engine::Whisper);
    assert_eq!(writing.recognition_terms.phrases(), Vec::<String>::new());
    assert!(!format!("{technical:?}").contains("Coding"));
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
        .push(Preset::capture("Earlier model".into(), &missing)?);
    assert_eq!(held.change_to(&draft), Change::Presentation);
    let mut file: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    file["future_setting"] = serde_json::json!("keep this edit");
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
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path)?)?["future_setting"],
        "keep this edit"
    );
    Ok(())
}

#[test]
fn old_files_need_no_presets_and_invalid_libraries_are_rejected_even_without_an_engine()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    std::fs::write(&path, "{}")?;
    assert_eq!(Config::read(&path)?.presets, Vec::<Preset>::new());
    let preset = serde_json::to_value(Preset::capture("PRIVATE_NAME".into(), &Config::default())?)?;
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
        assert!(matches!(
            Config::load(&path).1,
            super::super::Startup::Invalid(_)
        ));
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

proptest! {
    #[test]
    fn names_cannot_hide_case_duplicates_or_controls(name in "[a-z]{1,48}", control in 0u8..32) {
        let first = Preset::capture(name.clone(), &Config::default()).unwrap();
        let upper = Preset::capture(name.to_uppercase(), &Config::default()).unwrap();
        prop_assert!(validate(&[first, upper]).is_err());
        let corrupted = format!("{name}{}suffix", char::from(control));
        prop_assert!(Preset::capture(corrupted, &Config::default()).is_err());
    }
}
