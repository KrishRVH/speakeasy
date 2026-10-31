//! Native command acceptance uses a synthetic movie prefix without recording the desktop.

use std::{fs, os::windows::fs::OpenOptionsExt as _};

use super::*;

#[test]
#[ignore = "Opt in: native video decoder; set SPEAKEASY_MOVIE_READ_DIRECTORY to the validated synthetic movie prefixes"]
fn native_video_export_reports_real_samples_after_the_annotation_journal_ends() -> anyhow::Result<()>
{
    let fixtures = PathBuf::from(
        std::env::var_os("SPEAKEASY_MOVIE_READ_DIRECTORY")
            .context("Choose the synthetic movie prefix directory")?,
    );
    let directory = tempfile::tempdir()?;
    let show = directory.path().join("show");
    fs::create_dir(&show)?;
    let movie_path = show.join(movie::FILE);
    fs::copy(fixtures.join("moving.mp4"), &movie_path)?;
    let original = fs::read(&movie_path)?;
    fs::write(
        show.join(movie::HEADER),
        serde_json::to_vec(&serde_json::json!({
            "version":1,"originMs":250,"size":[1280,720],"finished":null,
            "coverage":{"endMs":4250,"frames":120,"bytes":original.len(),"annotationsThroughMs":1000,"partial":true}
        }))?,
    )?;
    fs::write(
        show.join("show.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":2,"duration":1000,"surface":{"width":1280,"height":720},
            "strokes":[],"pins":[],"actions":[],
            "marks":[{"number":1,"shape":"pin","point":[0.5,0.5],"bounds":[0.5,0.5,0.5,0.5],"span":[750,750],"strokes":[0,0]}],
            "narration":{"words":[{"span":[500,700],"text":"PRIVATE_PHRASE"}]},
            "keyframes":[{"index":1,"at":500,"reason":"middle","event":"PRIVATE_EVENT","marks":[0,0]}]
        }))?,
    )?;
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
    let served = reading.frame(Moment::from_millis(3755), || false)?.unwrap();
    assert_eq!(
        served.at.as_millis(),
        3750,
        "Decoder reopened the replaced path instead of retaining the validated file"
    );
    assert!(movie::Reading::open(&show, Moment::from_millis(1000)).is_err());
    Ok(())
}
