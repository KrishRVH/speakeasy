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
        parse(&["keep", "20261030T101010Z", "--off", "--json"])?,
        Command::Keep {
            directory: "20261030T101010Z".into(),
            kept: false,
            json: true,
        }
    );
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
        &["keep"],
        &["keep", "here", "--off", "--off"],
        &["keep", "here", "--json", "--json"],
        &["inspect", "here", "--off"],
        &["keep", "here", "extra"],
    ] {
        assert!(parse(args).is_err(), "accepted {args:?}");
    }
    Ok(())
}

#[test]
fn metadata_reports_exact_managed_identity_and_expiry_without_claiming_moved_bundles()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let managed = root.path().join("20261020T143205Z-2");
    Command::Demo {
        out: Some(managed.clone()),
    }
    .write(&mut io::sink())?;
    let mut printed = Vec::new();
    output::inspect(
        recall::Entry::open(&managed)?,
        Some(root.path()),
        true,
        &mut printed,
    )?;
    let value: Value = serde_json::from_slice(&printed)?;
    assert_eq!(value["id"], "20261020T143205Z-2");
    assert_eq!(value["expiresAt"], "2026-10-27T14:32:06Z");
    assert_eq!(value["pinned"], false);
    let elsewhere = tempfile::tempdir()?;
    printed.clear();
    output::inspect(
        recall::Entry::open(&managed)?,
        Some(elsewhere.path()),
        true,
        &mut printed,
    )?;
    let value: Value = serde_json::from_slice(&printed)?;
    assert!(value["id"].is_null() && value["expiresAt"].is_null());
    retention::set_kept(root.path(), &managed, true)?;
    printed.clear();
    output::inspect(
        recall::Entry::open(&managed)?,
        Some(root.path()),
        true,
        &mut printed,
    )?;
    let value: Value = serde_json::from_slice(&printed)?;
    assert_eq!(value["pinned"], true);
    assert!(value["expiresAt"].is_null());
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
            Some(root.path()),
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
            // Examples use double quotes for paths and phrases, without shell expansions.
            let args: Vec<_> = args
                .split('"')
                .enumerate()
                .flat_map(|(index, part)| {
                    if index.is_multiple_of(2) {
                        part.split_whitespace().collect::<Vec<_>>()
                    } else {
                        vec![part]
                    }
                })
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
        let error = output::list(std::iter::empty(), None, json, &mut Closed).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}

#[tokio::test]
async fn interrupted_listing_has_distinct_identity_and_original_expiry_without_replaying()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let (checkpoint, writer) = store::checkpoint::Keeper::fixture(
        root.path(),
        now,
        1_000,
        speakeasy_show::Size {
            width: 80.0,
            height: 60.0,
        },
    )?;
    assert!(writer.close().await);
    let checkpoint = checkpoint.close().await?;
    let interrupted = checkpoint.interrupted()?;
    // A header alone cannot be recovered, but metadata discovery must not read frames.
    let mut bytes = Vec::new();
    output::list(std::iter::empty(), Some(root.path()), true, &mut bytes)?;
    let listing: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(listing[0]["status"], "interrupted");
    assert_eq!(listing[0]["id"], interrupted.id());
    assert!(listing[0]["durationMs"].is_null());
    let expires = listing[0]["expiresAt"].as_str().unwrap();
    assert!(expires.contains(':') && expires.ends_with('Z'));
    assert_eq!(
        Command::parse(
            ["discard", interrupted.id().as_str(), "--json"]
                .map(OsString::from)
                .into_iter()
        )?,
        Command::Discard {
            directory: interrupted.id().into(),
            json: true
        }
    );
    // Metadata discovery must identify published audio without reading its private contents.
    let saved = store::fresh_directory(root.path(), now)?;
    fs::create_dir(&saved)?;
    let checkpoint_id = interrupted
        .directory()
        .file_name()
        .unwrap()
        .as_encoded_bytes();
    fs::write(saved.join(".checkpoint"), checkpoint_id)?;
    fs::write(
        interrupted.directory().join("published"),
        saved.file_name().unwrap().as_encoded_bytes(),
    )?;
    fs::write(
        interrupted.directory().join("audio.log"),
        b"private synthetic audio fixture",
    )?;
    bytes.clear();
    output::list(std::iter::empty(), Some(root.path()), true, &mut bytes)?;
    let listing: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(listing[0]["status"], "narrationPending");
    assert_eq!(listing[0]["id"], interrupted.id());
    assert!(!String::from_utf8(bytes)?.contains("private synthetic"));
    interrupted.lock()?.discard()?;
    assert!(
        saved.exists(),
        "Discarding pending audio removed its saved Show"
    );
    Ok(())
}

#[tokio::test]
async fn checkpoint_damage_is_reported_beside_readable_saved_shows() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let saved = root.path().join("20261030T010203Z");
    demo(Some(saved.clone()), &mut Vec::new())?;
    let (checkpoint, writer) = store::checkpoint::Keeper::fixture(
        root.path(),
        SystemTime::now(),
        1_000,
        speakeasy_show::Size {
            width: 80.0,
            height: 60.0,
        },
    )?;
    assert!(writer.close().await);
    let checkpoint = checkpoint.close().await?;
    let interrupted = checkpoint.interrupted()?;
    fs::write(interrupted.directory().join("published"), b"invalid marker")?;
    let mut bytes = Vec::new();
    output::list(
        recall::list(root.path())?,
        Some(root.path()),
        true,
        &mut bytes,
    )?;
    let listing: Vec<Value> = serde_json::from_slice(&bytes)?;
    assert_eq!(listing.len(), 2);
    assert!(
        listing.iter().any(|entry| entry["status"] == "damaged"
            && entry["problems"].as_array().unwrap().len() == 1)
    );
    assert!(
        listing.iter().any(|entry| entry["status"] == "ok"
            && entry["directory"] == saved.to_string_lossy().as_ref())
    );
    Ok(())
}
