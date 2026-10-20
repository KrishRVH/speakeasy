//! Checks the settings file contract, defaults, validation, and saves against temporary files.

use anyhow::bail;

use super::*;

fn saved(path: &Path) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

#[test]
fn a_new_file_skips_default_linux_keys_and_a_change_writes_only_its_key() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let defaults = Config::default();
    Config::save_changes(&defaults, &defaults, &defaults, &path)?;
    assert!(saved(&path)?.get("linux").is_none());

    let mut draft = defaults.clone();
    draft.linux.external_shortcut = true;
    Config::save_changes(&defaults, &draft, &draft, &path)?;
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
        bail!("The settings did not validate");
    };
    // Edited by hand while Speakeasy runs.
    fs::write(
        &path,
        r#"{"engine_executable":"engine","model":"model.gguf","threads":8}"#,
    )?;
    let draft = Config {
        theme: Theme::Iris,
        ..base.clone()
    };
    let validated = draft.validated(&path)?;
    Config::save_changes(&base, &draft, &validated, &path)?;
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
fn released_delivery_keys_read_and_save_unchanged() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    fs::write(
        &path,
        r#"{"engine_executable":"e","model":"m","preserve_clipboard":true}"#,
    )?;
    let base = Config::read(&path)?;
    assert_eq!(base.text_delivery(), TextDelivery::Type);
    let mut config = base.clone();
    config.set_text_delivery(TextDelivery::Paste);
    Config::save_changes(&base, &config, &config, &path)?;
    assert_eq!(
        saved(&path)?,
        serde_json::json!({"engine_executable": "e", "model": "m", "preserve_clipboard": false})
    );
    assert_eq!(Config::read(&path)?.text_delivery(), TextDelivery::Paste);

    #[cfg(target_os = "linux")]
    {
        let released = serde_json::json!({
            "engine_executable": "e",
            "model": "m",
            "preserve_clipboard": true,
            "linux": {
                "shortcut": "CTRL+ALT+d",
                "cancel": "CTRL+ALT+Escape",
                "terminal_paste": true,
                "manual_paste": false,
                "external_shortcut": false,
            },
        });
        fs::write(&path, released.to_string())?;
        let base = Config::read(&path)?;
        assert_eq!(base.text_delivery(), TextDelivery::Type);
        let mut config = base.clone();
        config.set_text_delivery(TextDelivery::Copy);
        Config::save_changes(&base, &config, &config, &path)?;
        let mut expected = released;
        expected["preserve_clipboard"] = false.into();
        expected["linux"]["manual_paste"] = true.into();
        assert_eq!(saved(&path)?, expected);
        let mut config = Config::read(&path)?;
        assert_eq!(config.text_delivery(), TextDelivery::Copy);
        config.set_text_delivery(TextDelivery::Type);
        assert!(config.linux.terminal_paste, "Typing forgot the paste keys");
        assert_eq!(config.text_delivery(), TextDelivery::Type);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn manual_paste_wins_over_keep_clipboard() {
    let config = Config {
        preserve_clipboard: true,
        linux: LinuxSettings {
            manual_paste: true,
            ..LinuxSettings::default()
        },
        ..Config::default()
    };
    assert_eq!(config.text_delivery(), TextDelivery::Copy);
    assert_eq!(config.delivery(), Delivery::Paste);
    assert!(config.desktop_options().manual_paste);
}

#[test]
fn shipped_example_is_the_wire_format() -> anyhow::Result<()> {
    const EXAMPLE: &str = include_str!("../../../../settings.example.json");
    let config: Config = serde_json::from_str(EXAMPLE)?;
    assert_eq!(
        serde_json::to_value(&config)?,
        serde_json::from_str::<serde_json::Value>(EXAMPLE)?
    );
    Ok(())
}

#[test]
fn engine_and_theme_spellings_are_the_wire_format() -> anyhow::Result<()> {
    for (engine, name) in [(Engine::Whisper, "whisper"), (Engine::Parakeet, "parakeet")] {
        assert_eq!(serde_json::to_value(engine)?, name);
        assert!(serde_json::from_value::<Engine>(name.into())? == engine);
    }
    for (theme, name) in [
        (Theme::Jet, "jet"),
        (Theme::Emerald, "emerald"),
        (Theme::Iris, "iris"),
        (Theme::Chrome, "chrome"),
    ] {
        assert_eq!(serde_json::to_value(theme)?, name);
        assert_eq!(serde_json::from_value::<Theme>(name.into())?, theme);
    }
    Ok(())
}

#[test]
fn omitted_keys_read_as_new_install_defaults() -> anyhow::Result<()> {
    let read = serde_json::from_str::<Config>(r#"{"engine_executable":"","model":""}"#)?;
    assert!(read == Config::default());
    Ok(())
}

#[test]
fn unknown_top_level_keys_are_ignored_but_linux_keys_stay_strict() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    fs::write(
        &path,
        r#"{"engine_executable":"e","model":"m","retired":1}"#,
    )?;
    let expected = Config {
        engine_executable: "e".into(),
        model: "m".into(),
        ..Config::default()
    };
    assert!(Config::read(&path)? == expected);
    fs::write(
        &path,
        r#"{"engine_executable":"e","model":"m","linux":{"shorcut":"X"}}"#,
    )?;
    let error = Config::read(&path)
        .err()
        .context("A misspelled Linux key was accepted")?;
    assert!(format!("{error:#}").contains("shorcut"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_save_writes_through_a_linked_settings_file() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let dotfiles = directory.path().join("dotfiles");
    fs::create_dir(&dotfiles)?;
    let real = dotfiles.join("settings.json");
    let path = directory.path().join("settings.json");
    let defaults = Config::default();
    Config::save_changes(&defaults, &defaults, &defaults, &real)?;
    std::os::unix::fs::symlink(&real, &path)?;
    let config = Config {
        theme: Theme::Iris,
        ..Config::default()
    };
    Config::save_changes(&defaults, &config, &config, &path)?;
    assert!(fs::symlink_metadata(&path)?.file_type().is_symlink());
    assert_eq!(Config::read(&real)?.theme, Theme::Iris);
    Ok(())
}

#[test]
fn save_refuses_to_replace_a_file_that_needs_repair() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    // A type error, not an unknown key, so the case holds whatever unknown keys are allowed.
    let broken = br#"{"engine_executable":"","model":"","threads":"four"}"#;
    fs::write(&path, broken)?;
    let draft = Config {
        theme: Theme::Iris,
        ..Config::default()
    };
    assert!(Config::save_changes(&Config::default(), &draft, &draft, &path).is_err());
    assert_eq!(fs::read(&path)?, broken);
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
    let config = Config {
        engine_executable: executable.clone(),
        model,
        ..Config::default()
    };
    let path = directory.path().join("settings.json");
    let failure = config
        .validated(&path)
        .err()
        .context("An engine without executable permission was accepted")?;
    assert!(failure.to_string().contains("not executable"));
    fs::set_permissions(executable, fs::Permissions::from_mode(0o700))?;
    config.validated(&path)?;
    Ok(())
}

#[test]
fn startup_classifies_saved_settings() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (config, startup) = Config::load(&path);
    assert!(matches!(startup, Startup::Unconfigured) && config == Config::default());

    fs::write(&path, "{")?;
    let (config, startup) = Config::load(&path);
    assert!(matches!(startup, Startup::Invalid(_)) && config == Config::default());

    // A file without an engine keeps the user's other choices for Settings to show.
    fs::write(&path, r#"{"theme":"iris"}"#)?;
    let (config, startup) = Config::load(&path);
    assert!(matches!(startup, Startup::Unconfigured));
    assert_eq!(config.theme, Theme::Iris);

    // A failed validation leaves the draft as the user wrote it.
    valid_config(directory.path())?;
    fs::write(
        &path,
        r#"{"engine_executable":"engine","model":"missing.gguf"}"#,
    )?;
    let (config, startup) = Config::load(&path);
    assert!(matches!(startup, Startup::Invalid(_)));
    assert_eq!(config.engine_executable, Path::new("engine"));

    fs::write(
        &path,
        r#"{"engine_executable":"engine","model":"model.gguf"}"#,
    )?;
    let (config, startup) = Config::load(&path);
    let Startup::Configured(valid) = startup else {
        bail!("Valid settings did not configure dictation");
    };
    assert_eq!(
        valid.engine_executable,
        directory.path().join("engine").canonicalize()?
    );
    assert!(config == *valid);
    Ok(())
}
