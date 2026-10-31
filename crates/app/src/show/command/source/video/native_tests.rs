//! Native command acceptance uses a synthetic movie prefix without recording the desktop.

use std::{fs, os::windows::fs::OpenOptionsExt as _};

use super::*;

mod annotations;

#[test]
#[ignore = "Opt in: native video decoder; set SPEAKEASY_MOVIE_GEOMETRY_DIRECTORY to synthetic geometry fixtures"]
fn native_mark_crops_match_presented_pixels_and_refuse_incompatible_spaces() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let show = directory.path().join("show");
    fs::create_dir(&show)?;
    movie::tests::native_geometry_fixture(&show)?;
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(show.join("show.json"))?)?;
    let run = |name: &str| -> anyhow::Result<serde_json::Value> {
        let args = [
            show.as_os_str().to_owned(),
            "--video".into(),
            "--mark".into(),
            "1".into(),
            "--crop".into(),
            "--out".into(),
            directory.path().join(name).into_os_string(),
            "--json".into(),
        ];
        let mut report = Vec::new();
        Command::parse(true, args.into_iter())?.write(&mut report)?;
        assert!(!String::from_utf8_lossy(&report).contains("PRIVATE"));
        Ok(serde_json::from_slice(&report)?)
    };
    for (at, part, [left, top, width, height], name) in [
        (284, [0.0, 0.25, 1.0, 0.75], [128, 108, 64, 24], "wide.png"),
        (750, [0.0, 0.0, 0.5, 1.0], [144, 96, 32, 48], "tall.png"),
    ] {
        manifest["marks"][0]["span"] = serde_json::json!([at, at]);
        manifest["pins"] = serde_json::json!([{"at":at,"point":[0.5,0.5]}]);
        manifest["spaces"] =
            serde_json::json!({"strokes":[],"pins":[{"generation":1,"part":part}]});
        fs::write(show.join("show.json"), serde_json::to_vec(&manifest)?)?;
        let report = run(name)?;
        assert_eq!(
            report["cells"][0]["cropPx"],
            serde_json::json!({"left":left,"top":top,"width":width,"height":height})
        );
        let raw = movie::Reading::open(&show, Moment::from_millis(1000))?
            .frame(Moment::from_millis(at), None, None, || false)?
            .unwrap();
        assert_eq!(
            fs::read(directory.path().join(name))?,
            raw.image.cropped(left, top, width, height)?.encode_png()?
        );
    }
    manifest["marks"][0]["span"] = serde_json::json!([349, 349]);
    manifest["pins"][0]["at"] = serde_json::json!(349);
    fs::write(show.join("show.json"), serde_json::to_vec(&manifest)?)?;
    assert!(run("mismatched.png").is_err());
    assert!(!directory.path().join("mismatched.png").exists());
    manifest["spaces"]["pins"][0] = serde_json::Value::Null;
    fs::write(show.join("show.json"), serde_json::to_vec(&manifest)?)?;
    assert!(run("unknown.png").is_err());
    assert!(!directory.path().join("unknown.png").exists());
    Ok(())
}

#[test]
#[ignore = "Opt in: native video decoder; set SPEAKEASY_MOVIE_READ_DIRECTORY to the validated synthetic movie prefixes"]
fn native_video_export_reports_real_samples_after_the_annotation_journal_ends() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let show = directory.path().join("show");
    fs::create_dir(&show)?;
    movie::tests::native_fixture(&show)?;
    let movie_path = show.join(movie::FILE);
    let original = fs::read(&movie_path)?;
    let run = |option: &str, value: &str, name: &str| -> anyhow::Result<serde_json::Value> {
        let output = directory.path().join(name);
        let args = [
            show.as_os_str().to_owned(),
            "--video".into(),
            option.into(),
            value.into(),
            "--out".into(),
            output.into_os_string(),
            "--json".into(),
        ];
        let command = Command::parse(true, args.into_iter())?;
        let mut report = Vec::new();
        command.write(&mut report)?;
        assert!(!String::from_utf8_lossy(&report).contains("PRIVATE"));
        Ok(serde_json::from_slice(&report)?)
    };
    let tail = run("--at", "3.755", "tail.png")?;
    assert_eq!(tail["requestedMs"], 3755);
    assert_eq!(tail["sampledMs"], 3750);
    assert_eq!(tail["video"]["rangeMs"], serde_json::json!([250, 4250]));
    assert_eq!(tail["video"]["annotationsThroughMs"], 1000);
    let png = fs::read(directory.path().join("tail.png"))?;
    assert!(png.starts_with(b"\x89PNG"));
    assert!(run("--at", "1", "tail.png").is_err());
    assert_eq!(fs::read(directory.path().join("tail.png"))?, png);
    assert_eq!(run("--at", "0.250", "first.png")?["sampledMs"], 250);
    assert_eq!(run("--at", "4.250", "last.png")?["sampledMs"], 4216);
    for (option, value, name, requested) in [
        ("--mark", "1", "mark.png", 750_u64),
        ("--phrase", "PRIVATE_PHRASE", "phrase.png", 500),
    ] {
        let report = run(option, value, name)?;
        let actual = report["sampledMs"].as_u64().unwrap();
        assert!(actual <= requested && requested - actual < 34);
    }
    for value in ["0.249", "4.251"] {
        assert!(run("--at", value, "invalid.png").is_err());
        assert!(!directory.path().join("invalid.png").exists());
    }
    assert_eq!(fs::read(&movie_path)?, original);
    let reading = movie::Reading::open(&show, Moment::from_millis(1000))?;
    assert!(
        fs::OpenOptions::new()
            .write(true)
            .share_mode(7)
            .open(&movie_path)
            .is_err()
    );
    fs::rename(&movie_path, show.join("validated.mp4"))?;
    fs::write(&movie_path, vec![0; original.len()])?;
    let served = reading
        .frame(Moment::from_millis(3755), None, None, || false)?
        .unwrap();
    assert_eq!(
        served.at.as_millis(),
        3750,
        "Decoder reopened the replaced path instead of retaining the validated file"
    );
    assert!(movie::Reading::open(&show, Moment::from_millis(1000)).is_err());
    Ok(())
}
