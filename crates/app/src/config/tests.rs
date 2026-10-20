//! Checks the settings file contract, defaults, validation, and saves against temporary files.

use anyhow::bail;

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

#[test]
fn save_refuses_to_replace_a_file_that_needs_repair() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    // A type error, not an unknown key, so the case holds whatever unknown keys are allowed.
    let broken = br#"{"engine_executable":"","model":"","threads":"four"}"#;
    fs::write(&path, broken)?;
    assert!(Config::default().save(&path).is_err());
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
