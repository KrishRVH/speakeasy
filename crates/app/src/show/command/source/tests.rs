use std::fs;

use speakeasy_show::{Image, Journal};

use super::*;

fn retain(directory: &Path, journal: &Journal, duration: u64) -> anyhow::Result<()> {
    let mut encoded = Vec::new();
    journal.write_source::<speakeasy_show::SourceError>(
        Moment::from_millis(duration),
        |bytes| {
            encoded.extend_from_slice(bytes);
            Ok(())
        },
    )?;
    fs::write(directory.join(source::FILE), encoded)?;
    Ok(())
}

#[test]
fn source_arguments_need_one_focus_and_a_destination_for_files() {
    for args in [
        vec!["show"],
        vec!["show", "--at", "1"],
        vec!["show", "--out", "x"],
        vec!["show", "--at", "1", "--at", "2", "--out", "x"],
        vec!["show", "--at", "1", "--out", "--video"],
        vec!["show", "--at", "1", "--out", "x", "--annotated"],
        vec![
            "show",
            "--at",
            "1",
            "--out",
            "x",
            "--video",
            "--annotated",
            "--annotated",
        ],
    ] {
        assert!(parse(Verb::Frame, args.into_iter().map(OsString::from)).is_err());
    }
    assert!(
        parse(
            Verb::Frames,
            ["show", "--at", "1"].into_iter().map(OsString::from)
        )
        .is_ok()
    );
    let args = ["show", "--at", "1", "--out", "x", "--annotated", "--video"];
    assert!(parse(Verb::Frame, args.into_iter().map(OsString::from)).is_ok());
    assert!(parse(Verb::Frames, args.into_iter().map(OsString::from)).is_err());
}

#[test]
fn source_commands_report_samples_preserve_files_and_handle_closed_pipes() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("show");
    fs::create_dir(&directory)?;
    fs::write(
        directory.join("show.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":3, "duration":1000, "surface":{"width":2,"height":2},
            "strokes":[],"pins":[],"actions":[],"marks":[],"narration":{"words":[]},
            "keyframes":[{"index":1,"at":100,"reason":"middle","event":"PRIVATE_EVENT","marks":[0,0]}]
        }))?,
    )?;
    let mut journal = Journal::new();
    journal.admit().unwrap().record(
        Moment::from_millis(100),
        Image::from_bgra(2, 2, [123, 123, 123, 255].repeat(4))?,
    );
    retain(&directory, &journal, 1_000)?;
    let request = |read| Request {
        directory: directory.clone(),
        read,
        json: true,
    };
    for read in [
        Read::Coverage,
        Read::Source {
            focus: Query::At(Moment::from_millis(750)),
            around: AROUND,
            output: Output::List,
        },
    ] {
        let error = request(read)
            .write(&mut crate::testing::Closed)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::BrokenPipe)
        );
    }
    let destination = root.path().join("export.png");
    let frame = || {
        request(Read::Source {
            focus: Query::At(Moment::from_millis(750)),
            around: AROUND,
            output: Output::Frame {
                destination: destination.clone(),
                crop: None,
            },
        })
    };
    let mut output = Vec::new();
    frame().write(&mut output)?;
    let value: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(value["requestedMs"], 750);
    assert_eq!(value["sampledMs"], 100);
    let png = fs::read(&destination)?;
    assert!(png.starts_with(b"\x89PNG"));
    assert!(frame().write(&mut io::sink()).is_err());
    assert_eq!(fs::read(destination)?, png);
    output.clear();
    request(Read::Coverage).write(&mut output)?;
    let value: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(value["retained"], 1);
    assert_eq!(value["longestGapMs"], 900);
    Ok(())
}

#[test]
fn phrase_windows_require_an_explicit_match_and_export_honest_samples() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("show");
    fs::create_dir(&directory)?;
    fs::write(
        directory.join("show.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":3, "duration":4000, "surface":{"width":100,"height":100},
            "strokes":[],"pins":[],"actions":[],
            "marks":[{"number":1,"shape":"pin","point":[0.5,0.5],"bounds":[0.5,0.5,0.5,0.5],"span":[2500,2500],"strokes":[0,0]}],
            "narration":{"words":[{"span":[400,800],"text":"PRIVATE_PHRASE"},{"span":[2500,2700],"text":"PRIVATE_PHRASE"}]},
            "keyframes":[{"index":1,"at":2500,"reason":"mark","event":"PRIVATE_EVENT","marks":[0,1]}]
        }))?,
    )?;
    let mut journal = Journal::new();
    for at in [100, 1200, 2000, 3200] {
        journal.admit().unwrap().record(
            Moment::from_millis(at),
            Image::from_bgra(100, 100, [1, 2, 3, 255].repeat(10_000))?,
        );
    }
    retain(&directory, &journal, 4_000)?;
    let run = |options: &[&str], verb| -> anyhow::Result<serde_json::Value> {
        let args = std::iter::once(directory.as_os_str().to_owned())
            .chain(options.iter().map(OsString::from));
        let command = parse(verb, args)?;
        assert!(!format!("{command:?}").contains("PRIVATE"));
        let mut out = Vec::new();
        command.write(&mut out)?;
        assert!(!String::from_utf8_lossy(&out).contains("PRIVATE"));
        Ok(serde_json::from_slice(&out)?)
    };
    let found = run(&["--phrase", "PRIVATE_PHRASE", "--json"], Verb::Frames)?;
    assert_eq!(
        found["focus"]["matches"],
        serde_json::json!([[400, 800], [2500, 2700]])
    );
    assert!(found["windowMs"].is_null());
    let destination = root.path().join("sheet.png");
    let path = destination.to_str().unwrap();
    let error = run(
        &["--phrase", "PRIVATE_PHRASE", "--out", path, "--json"],
        Verb::Frames,
    )
    .unwrap_err();
    assert!(error.to_string().contains("several matches"));
    assert!(!error.to_string().contains("PRIVATE"));
    assert!(!destination.exists());
    let exported = run(
        &[
            "--phrase",
            "PRIVATE_PHRASE",
            "--match",
            "2",
            "--out",
            path,
            "--most",
            "2",
            "--json",
        ],
        Verb::Frames,
    )?;
    assert_eq!(exported["windowMs"], serde_json::json!([1500, 3700]));
    assert_eq!(exported["carriedInMs"], 1200);
    assert_eq!(exported["timesMs"], serde_json::json!([1200, 2000]));
    assert_eq!(exported["cells"].as_array().unwrap().len(), 2);
    assert_eq!(exported["longestGapMs"], 1200);
    let png = fs::read(&destination)?;
    assert!(run(&["--at", "2", "--out", path, "--json"], Verb::Frame).is_err());
    assert_eq!(fs::read(&destination)?, png);
    let crop = root.path().join("crop.png");
    let cropped = run(
        &[
            "--mark",
            "1",
            "--crop",
            "--out",
            crop.to_str().unwrap(),
            "--json",
        ],
        Verb::Frame,
    )?;
    assert_eq!(cropped["requestedMs"], 2500);
    assert_eq!(cropped["sampledMs"], 2000);
    assert_eq!(cropped["cells"][0]["cropPx"]["left"], 40);
    let manifest_path = directory.join("show.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    manifest["marks"][0]["span"] = serde_json::json!([1500, 3000]);
    fs::write(manifest_path, serde_json::to_vec(&manifest)?)?;
    let mut resized = Journal::new();
    for (at, width) in [(1000, 100), (2000, 200), (3000, 100)] {
        resized.admit().unwrap().record(
            Moment::from_millis(at),
            Image::from_bgra(
                width,
                100,
                [1, 2, 3, 255].repeat(usize::try_from(width)?.saturating_mul(100)),
            )?,
        );
    }
    retain(&directory, &resized, 4_000)?;
    let destination = root.path().join("resized.png");
    let error = run(
        &[
            "--mark",
            "1",
            "--crop",
            "--out",
            destination.to_str().unwrap(),
            "--json",
        ],
        Verb::Frame,
    )
    .unwrap_err();
    assert!(error.to_string().contains("resized"));
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn focus_options_reject_conflicts_and_unbounded_work() {
    for options in [
        vec!["--phrase", "PRIVATE", "--at", "1"],
        vec!["--phrase", "--json"],
        vec!["--at", "1", "--crop", "--out", "x.png"],
        vec!["--at", "1", "--match", "1"],
        vec!["--at", "1", "--most", "13", "--out", "x.png"],
        vec!["--at", "1", "--around", "10001"],
        vec!["--at", "1", "--most", "0", "--out", "x.png"],
        vec!["--at", "1", "--most", "2"],
        vec!["--out", "x.png"],
        vec!["--video", "--at", "1", "--out", "x.png"],
    ] {
        let args = std::iter::once("show").chain(options).map(OsString::from);
        let error = parse(Verb::Frames, args).unwrap_err();
        assert!(!format!("{error:#}").contains("PRIVATE"));
    }
    let repeated = ["show", "--video", "--video", "--at", "1", "--out", "x.png"];
    assert!(parse(Verb::Frame, repeated.into_iter().map(OsString::from)).is_err());
    assert!(
        parse(
            Verb::Frame,
            ["show", "--video", "--mark", "1", "--crop", "--out", "x.png"]
                .into_iter()
                .map(OsString::from)
        )
        .is_ok()
    );
}
