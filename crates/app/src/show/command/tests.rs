//! Command acceptance and metadata output on synthetic Shows, without native devices.

use std::fs;

use serde_json::Value;
use speakeasy_show::Bundle;

use super::*;

fn parse(args: &[&str]) -> anyhow::Result<Command> {
    Command::parse(args.iter().map(OsString::from))
}

#[test]
fn commands_require_explicit_paths_and_accept_only_their_options() -> anyhow::Result<()> {
    assert_eq!(parse(&[])?, Command::Help);
    assert_eq!(parse(&["--help"])?, Command::Help);
    assert_eq!(parse(&["list", "--json"])?, Command::List { json: true });
    assert_eq!(
        parse(&["inspect", "a show", "--json"])?,
        Command::Inspect {
            directory: "a show".into(),
            json: true
        }
    );
    assert_eq!(parse(&["skill"])?, Command::Skill);
    assert_eq!(
        parse(&["demo", "--out", "here"])?,
        Command::Demo {
            out: Some("here".into())
        }
    );
    for args in [
        &["inspect"][..],
        &["inspect", ""],
        &["inspect", "here", "extra"],
        &["list", "--json", "--json"],
        &["list", "--out", "here"],
        &["demo", "--json"],
        &["demo", "--out"],
        &["skill", "extra"],
        &["record"],
    ] {
        assert!(parse(args).is_err(), "accepted {args:?}");
    }
    Ok(())
}

#[test]
fn a_demo_is_inspectable_and_output_never_contains_its_narration() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("demo with spaces");
    Command::Demo {
        out: Some(directory.clone()),
    }
    .write(&mut io::sink())?;
    let summary = directory.join(Bundle::SUMMARY);
    let edited = b"PRIVATE_USER_EDIT";
    fs::write(&summary, edited)?;
    for json in [false, true] {
        let mut output = Vec::new();
        Command::Inspect {
            directory: directory.clone(),
            json,
        }
        .write(&mut output)?;
        let output = String::from_utf8(output)?;
        let mut listing = Vec::new();
        output::list(
            std::iter::once(recall::Entry::open(&directory)),
            json,
            &mut listing,
        )?;
        let listing = String::from_utf8(listing)?;
        for private in [
            "PRIVATE_USER_EDIT",
            "Here's the settings window",
            "theme picker is here",
            "reduce motion",
        ] {
            assert!(
                !output.contains(private) && !listing.contains(private),
                "inspection quoted private content"
            );
        }
        if json {
            let value: Value = serde_json::from_str(&output)?;
            assert_eq!(value["status"], "ok");
            assert_eq!(value["keyframes"], 4);
            assert_eq!(value["marks"], 3);
            assert_eq!(value["markFrames"][0]["frame"], 2);
            assert_eq!(
                value["sheets"][0]["frames"],
                serde_json::json!([1, 2, 3, 4])
            );
            for frame in value["frames"].as_array().unwrap() {
                assert!(PathBuf::from(frame["path"].as_str().unwrap()).is_file());
            }
        }
    }
    assert_eq!(fs::read(summary)?, edited);
    Ok(())
}

#[test]
fn inspection_reports_damage_before_failing_and_does_not_repair_it() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("show");
    Command::Demo {
        out: Some(directory.clone()),
    }
    .write(&mut io::sink())?;
    let missing = directory.join("frames/frame-01.png");
    fs::remove_file(&missing)?;
    let mut output = Vec::new();
    assert!(
        Command::Inspect {
            directory: directory.clone(),
            json: true
        }
        .write(&mut output)
        .is_err()
    );
    let value: Value = serde_json::from_slice(&output)?;
    assert_eq!(value["status"], "damaged");
    assert_eq!(value["frames"].as_array().unwrap().len(), 4);
    assert!(!missing.exists());
    fs::write(directory.join("show.json"), br#"{"version":99}"#)?;
    output.clear();
    assert!(
        Command::Inspect {
            directory,
            json: true
        }
        .write(&mut output)
        .is_err()
    );
    let value: Value = serde_json::from_slice(&output)?;
    assert_eq!(value["status"], "unsupported");
    assert!(value["keyframes"].is_null());
    assert!(value["pinned"].is_null());
    assert!(value.get("frames").is_none());
    Ok(())
}

#[test]
fn the_embedded_skill_is_emitted_verbatim_and_its_commands_are_accepted() -> anyhow::Result<()> {
    let mut output = Vec::new();
    Command::Skill.write(&mut output)?;
    let skill = String::from_utf8(output)?;
    let mut commands = 0;
    for span in skill.split('`').skip(1).step_by(2) {
        if let Some(args) = span.strip_prefix("speakeasy show ") {
            // The skill's one quoted placeholder is a single, space-free bundle argument.
            let args: Vec<_> = args
                .split_whitespace()
                .map(|arg| arg.trim_matches('"'))
                .collect();
            parse(&args)?;
            commands += 1;
        }
    }
    assert!(
        commands > 0,
        "The emitted skill contains no usable commands"
    );
    Ok(())
}

#[test]
fn closing_the_output_pipe_preserves_the_io_error_kind() {
    struct Closed;
    impl io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    for json in [false, true] {
        let error = output::list(std::iter::empty(), json, &mut Closed).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}
