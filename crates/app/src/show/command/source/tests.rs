//! Source commands require explicit times and new destinations and report the actual served time.

use std::fs;

use speakeasy_show::{Image, Journal};

use super::*;

#[test]
fn source_arguments_reject_ambiguous_times_and_missing_destinations() -> anyhow::Result<()> {
    for (text, ms) in [("0", 0), ("12.5", 12_500), ("1:02.005", 62_005)] {
        assert_eq!(time(text)?.as_millis(), ms);
    }
    for text in [
        "",
        "-1",
        "NaN",
        "1:60",
        "1:2:3",
        "1.",
        ".1",
        "1.1234",
        "1e3",
        "18446744073709551615",
    ] {
        assert!(time(text).is_err(), "accepted {text}");
    }
    for args in [
        vec!["show"],
        vec!["show", "--at", "1"],
        vec!["show", "--out", "x"],
        vec!["show", "--at", "1", "--at", "2", "--out", "x"],
    ] {
        assert!(Command::parse(true, args.into_iter().map(OsString::from)).is_err());
    }
    assert!(Command::parse(false, ["show", "--at", "1"].into_iter().map(OsString::from)).is_err());
    Ok(())
}

#[test]
fn exported_frame_reports_actual_time_without_overwriting_or_printing_content() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let directory = root.path().join("show");
    fs::create_dir(&directory)?;
    let mut journal = Journal::new();
    journal.admit().unwrap().record(
        Moment::from_millis(100),
        Image::from_bgra(2, 2, [123, 123, 123, 255].repeat(4))?,
    );
    // The app's staging writer is deliberately private; encode a public synthetic source fixture.
    let mut encoded = Vec::new();
    journal.write_source::<speakeasy_show::SourceError>(Moment::from_millis(1_000), |bytes| {
        encoded.extend_from_slice(bytes);
        Ok(())
    })?;
    fs::write(directory.join(source::FILE), encoded)?;
    let destination = root.path().join("export.png");
    let command = || Command::Frame {
        directory: directory.clone(),
        at: Moment::from_millis(750),
        destination: destination.clone(),
        json: true,
    };
    let mut output = Vec::new();
    command().write(&mut output)?;
    let value: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(value["requestedMs"], 750);
    assert_eq!(value["sampledMs"], 100);
    let png = fs::read(&destination)?;
    assert!(png.starts_with(b"\x89PNG"));
    assert!(command().write(&mut io::sink()).is_err());
    assert_eq!(fs::read(destination)?, png);
    output.clear();
    Command::Frames {
        directory,
        json: true,
    }
    .write(&mut output)?;
    let value: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(value["retained"], 1);
    assert_eq!(value["longestGapMs"], 900);
    Ok(())
}
