use anyhow::bail;

use super::*;

#[test]
fn managed_model_identity_cannot_follow_a_different_file_and_hint_edits_keep_the_worker()
-> anyhow::Result<()> {
    let mut base = Config {
        engine: Engine::Nemo,
        nemo_model: Some(Model::Nemotron),
        model: Model::Nemotron.file_name().into(),
        ..Config::default()
    };
    assert!(base.uses_nemotron());
    let mut edited = base.clone();
    edited.recognition_terms.add("Example Name")?;
    assert_eq!(base.change_to(&edited), Change::Session);
    base.model = Model::Parakeet.file_name().into();
    assert!(!base.uses_nemotron());
    Ok(())
}

fn saved(path: &Path) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn save(held: &Config, draft: &Config, path: &Path) -> anyhow::Result<()> {
    Config::save_changes(&[held], draft, &ValidConfig::assume(draft.clone()), path)
}

#[test]
fn a_new_file_skips_default_linux_keys_and_a_change_writes_only_its_key() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let defaults = Config::default();
    save(&defaults, &defaults, &path)?;
    assert!(saved(&path)?.get("linux").is_none());

    let mut draft = defaults.clone();
    draft.linux.external_shortcut = true;
    save(&defaults, &draft, &path)?;
    assert_eq!(
        saved(&path)?["linux"],
        serde_json::json!({"external_shortcut": true})
    );
    assert_eq!(Config::read(&path)?.linux, draft.linux);
    Ok(())
}

#[test]
fn a_save_keeps_hand_edits_and_path_spellings() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    valid_config(directory.path())?;
    fs::write(
        &path,
        r#"{"engine_executable":"engine","model":"model.gguf","threads":4}"#,
    )?;
    let (base, Startup::Configured(_)) = Config::load(&path) else {
        bail!("The settings didn't validate");
    };

    fs::write(
        &path,
        r#"{"engine_executable":"engine","model":"model.gguf","threads":8}"#,
    )?;
    let draft = Config {
        theme: Theme::Iris,
        ..base.clone()
    };
    let validated = draft.validated(&path)?;
    Config::save_changes(&[&base], &draft, &validated, &path)?;
    let saved = saved(&path)?;
    let keys: Vec<_> = saved.as_object().into_iter().flat_map(Map::keys).collect();
    assert_eq!(keys, ["engine_executable", "model", "threads", "theme"]);
    assert_eq!(
        saved,
        serde_json::json!({
            "engine_executable": "engine",
            "model": "model.gguf",
            "threads": 8,
            "theme": "iris",
        })
    );
    Ok(())
}

#[test]
fn filler_cleanup_follows_language_and_switch() -> anyhow::Result<()> {
    for (language, applies) in [
        ("en", true),
        ("EN", true),
        ("en-US", true),
        ("en-GB", true),
        ("EN-au", true),
        ("de-en", false),
        ("de", false),
        ("auto", false),
    ] {
        for remove_fillers in [true, false] {
            let config = Config {
                language: language.into(),
                remove_fillers,
                ..Config::default()
            };
            let expected = if applies && remove_fillers {
                Fillers::Remove
            } else {
                Fillers::Preserve
            };
            assert_eq!(
                config.fillers(&config.language),
                expected,
                "{language}, switch {remove_fillers}"
            );
        }
    }
    let without_switch: Config = serde_json::from_str(r#"{"engine":"nemo","language":"de"}"#)?;
    assert_eq!(
        without_switch.fillers(&without_switch.language),
        Fillers::Preserve
    );
    Ok(())
}

#[test]
fn delivery_reconfigures_only_the_owner_that_changes() {
    let base = Config::default();
    for (text_delivery, delivery, paste, change) in [
        (
            TextDelivery::Paste,
            Delivery::Paste,
            LinuxPaste::Standard,
            Change::Unchanged,
        ),
        (
            TextDelivery::Type,
            Delivery::Direct,
            LinuxPaste::Standard,
            Change::Session,
        ),
        #[cfg(target_os = "linux")]
        (
            TextDelivery::TerminalPaste,
            Delivery::Paste,
            LinuxPaste::Terminal,
            Change::Desktop,
        ),
        #[cfg(target_os = "linux")]
        (
            TextDelivery::Copy,
            Delivery::Paste,
            LinuxPaste::Manual,
            Change::Desktop,
        ),
    ] {
        let config = Config {
            text_delivery,
            ..base.clone()
        };
        assert_eq!(base.change_to(&config), change);
        assert_eq!(config.delivery(), delivery);
        assert_eq!(config.desktop_options().paste, paste);
        assert_eq!(config.change_to(&base), change);
    }
}

#[test]
fn shipped_example_is_what_a_first_launch_starts_from() -> anyhow::Result<()> {
    const EXAMPLE: &str = include_str!("../../../../settings.example.json");
    let config: Config = serde_json::from_str(EXAMPLE)?;
    assert_eq!(config, Config::default());
    assert_eq!(
        serde_json::to_value(&config)?,
        serde_json::from_str::<Value>(EXAMPLE)?
    );
    Ok(())
}

#[test]
fn starting_at_login_reaches_neither_a_session_nor_the_engine() {
    let next = Config {
        start_at_login: true,
        ..Config::default()
    };
    assert_eq!(Config::default().change_to(&next), Change::Presentation);
}

#[test]
fn omitted_keys_read_as_the_defaults() -> anyhow::Result<()> {
    let read = serde_json::from_str::<Config>("{}")?;
    assert_eq!(read, Config::default());
    Ok(())
}

#[test]
fn unsupported_settings_are_rejected_without_overwriting_the_file() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let base = Config::default();
    let draft = Config {
        theme: Theme::Iris,
        ..base.clone()
    };
    for contents in [
        r#"{"retired":1}"#,
        r#"{"engine":"parakeet"}"#,
        r#"{"linux":{"chord":"X"}}"#,
    ] {
        fs::write(&path, contents)?;
        assert!(Config::read(&path).is_err(), "{contents}");
        assert!(save(&base, &draft, &path).is_err(), "{contents}");
        assert_eq!(fs::read(&path)?, contents.as_bytes());
    }
    Ok(())
}

#[test]
fn save_refuses_to_replace_a_file_that_needs_repair() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");

    let broken = br#"{"threads":"four"}"#;
    fs::write(&path, broken)?;
    let draft = Config {
        theme: Theme::Iris,
        ..Config::default()
    };
    let error = save(&Config::default(), &draft, &path)
        .err()
        .context("A file that needs repair was replaced")?;
    assert_eq!(fs::read(&path)?, broken);

    let notice = format!("Invalid settings in {}:", path.display());
    assert!(format!("{error:#}").starts_with(&notice));
    Ok(())
}

#[test]
fn a_save_that_cannot_write_names_the_file_and_what_to_do() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let blocked = directory.path().join("speakeasy");
    fs::write(&blocked, b"a file where the folder belongs")?;
    let path = blocked.join("settings.json");
    let error = save(&Config::default(), &Config::default(), &path)
        .err()
        .context("Settings were saved beneath a file")?;
    let message = error.to_string();
    assert!(
        message.starts_with(&format!("Can't save settings to {}: ", path.display()))
            && message.ends_with(", then try again."),
        "{message}"
    );
    Ok(())
}

fn start_from(contents: Option<&str>) -> anyhow::Result<(tempfile::TempDir, Config, Startup)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    valid_config(directory.path())?;
    if let Some(contents) = contents {
        fs::write(&path, contents)?;
    }
    let (config, startup) = Config::load(&path);
    Ok((directory, config, startup))
}

#[test]
fn a_new_install_starts_unconfigured_from_the_defaults() -> anyhow::Result<()> {
    let (_directory, config, startup) = start_from(None)?;
    assert!(matches!(startup, Startup::Unconfigured));
    assert_eq!(config, Config::default());
    Ok(())
}

#[test]
fn settings_that_do_not_parse_start_invalid_from_the_defaults() -> anyhow::Result<()> {
    let (_directory, config, startup) = start_from(Some("{"))?;
    assert!(matches!(startup, Startup::Invalid(_)));
    assert_eq!(config, Config::default());
    Ok(())
}

#[test]
fn settings_without_an_engine_start_unconfigured_with_their_other_choices() -> anyhow::Result<()> {
    let (_directory, config, startup) = start_from(Some(r#"{"theme":"iris"}"#))?;
    assert!(matches!(startup, Startup::Unconfigured));
    assert_eq!(
        config.theme,
        Theme::Iris,
        "A file without an engine lost the user's other choices"
    );
    assert_eq!(
        config.engine,
        Engine::Nemo,
        "An omitted engine key didn't use the current default"
    );
    Ok(())
}

#[test]
fn settings_naming_a_missing_file_start_invalid_as_written() -> anyhow::Result<()> {
    let (_directory, config, startup) = start_from(Some(
        r#"{"engine_executable":"engine","model":"missing.gguf"}"#,
    ))?;
    assert!(matches!(startup, Startup::Invalid(_)));
    assert_eq!(
        config.engine_executable,
        Path::new("engine"),
        "A failed validation rewrote the draft"
    );
    Ok(())
}

#[test]
fn valid_settings_configure_dictation_with_resolved_paths() -> anyhow::Result<()> {
    let (directory, config, startup) = start_from(Some(
        r#"{"engine_executable":"engine","model":"model.gguf"}"#,
    ))?;
    let Startup::Configured(valid) = startup else {
        bail!("Valid settings didn't configure dictation");
    };
    assert_eq!(
        valid.engine_executable,
        directory.path().join("engine").canonicalize()?
    );
    assert_eq!(config, **valid);
    Ok(())
}

#[test]
fn vocabulary_saves_preserve_other_hand_edits_and_require_session_reconfiguration()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let base = valid_config(directory.path())?;
    save(&base, &base, &path)?;
    let mut file = saved(&path)?;
    file["threads"] = 8.into();
    fs::write(&path, serde_json::to_vec(&file)?)?;
    let mut draft = base.clone();
    draft.vocabulary.push(Correction {
        heard: "PRIVATE_PHRASE".into(),
        written: "private_name".into(),
    });
    assert_eq!(base.change_to(&draft), Change::Session);
    Config::save_changes(&[&base], &draft, &draft.validated(&path)?, &path)?;
    let stored = saved(&path)?;
    assert_eq!(stored["threads"], 8);
    assert_eq!(Config::read(&path)?.vocabulary, draft.vocabulary);
    let mut external = stored;
    external["vocabulary"][0]["written"] = "Hand edited".into();
    fs::write(&path, serde_json::to_vec(&external)?)?;
    let next = Config {
        theme: Theme::Iris,
        ..draft.clone()
    };
    Config::save_changes(&[&draft], &next, &next.validated(&path)?, &path)?;
    assert_eq!(saved(&path)?["vocabulary"][0]["written"], "Hand edited");
    Ok(())
}

#[test]
fn removing_the_last_one_press_shortcut_removes_it_from_the_file() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let base = valid_config(directory.path())?;
    save(&base, &base, &path)?;
    let mut set = base.clone();
    set.windows_toggles.hands_free = Some("control+shift+h".parse().map_err(anyhow::Error::msg)?);
    Config::save_changes(&[&base], &set, &set.validated(&path)?, &path)?;
    assert!(saved(&path)?.get("windows_toggles").is_some());
    Config::save_changes(&[&set], &base, &base.validated(&path)?, &path)?;
    assert_eq!(
        saved(&path)?.get("windows_toggles"),
        None,
        "The removed shortcut stayed in the file and would return on the next start"
    );
    Ok(())
}

#[test]
fn a_settings_file_whose_shortcut_types_or_repeats_another_is_refused() {
    let typing = Config {
        mac_toggles: Toggles {
            hands_free: "k".parse().ok(),
            ..Toggles::default()
        },
        windows_toggles: Toggles {
            hands_free: "k".parse().ok(),
            ..Toggles::default()
        },
        ..Config::default()
    };
    let error = typing
        .validate_choices()
        .expect_err("a hands-free K would swallow typing");
    assert!(
        error
            .to_string()
            .starts_with("Choose another Hands-free shortcut in Settings."),
        "{error}"
    );
    let repeated = Config {
        mac_toggles: Toggles {
            show_area: Some(MacShortcut::F13),
            ..Toggles::default()
        },
        windows_toggles: Toggles {
            show_area: Some(WindowsShortcut::F13),
            ..Toggles::default()
        },
        mac_shortcut: MacShortcut::F13,
        windows_shortcut: WindowsShortcut::F13,
        ..Config::default()
    };
    assert!(
        repeated.validate_choices().is_err(),
        "Two shortcuts shared F13"
    );
    assert!(Config::default().validate_choices().is_ok());
}
