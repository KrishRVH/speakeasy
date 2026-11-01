use std::{fs, path::Path};

use super::{
    super::tests::{fixture, fixture_manifest},
    *,
};

fn content(result: &Value) -> anyhow::Result<Value> {
    assert_eq!(result["isError"], false, "{result}");
    Ok(serde_json::from_str(
        result["content"][0]["text"].as_str().unwrap(),
    )?)
}

fn snapshot(directory: &Path) -> anyhow::Result<Vec<(std::path::PathBuf, Vec<u8>)>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            found.extend(snapshot(&entry.path())?);
        } else {
            found.push((entry.path(), fs::read(entry.path())?));
        }
    }
    found.sort_by(|one, two| one.0.cmp(&two.0));
    Ok(found)
}

#[test]
fn reads_preserve_edits_and_every_file_and_only_explicitly_expose_content() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path(), "exact")?;
    let before = snapshot(root.path())?;
    let roots = Roots::open(vec![root.path().to_owned()]).map_err(anyhow::Error::msg)?;
    let listed = call(&roots, Tool::List, json!({}));
    assert_eq!(content(&listed)?["shows"].as_array().unwrap().len(), 1);
    assert!(!serde_json::to_string(&listed)?.contains("PRIVATE_"));
    let read = content(&call(&roots, Tool::Read, json!({"show": "exact"})))?;
    assert_eq!(read["summary"]["text"], "PRIVATE_EDITED_SUMMARY");
    assert!(read["narration"].is_null());
    assert!(!serde_json::to_string(&read)?.contains("PRIVATE_NARRATION"));
    let read = content(&call(
        &roots,
        Tool::Read,
        json!({"show": directory, "narration": true, "phrase": "PRIVATE_NARRATION"}),
    ))?;
    assert_eq!(read["narration"]["kind"], "timed");
    assert_eq!(read["location"]["matches"].as_array().unwrap().len(), 2);
    assert!(read["location"]["startMs"].is_null());
    assert_eq!(read["narration"]["words"][0]["text"], "PRIVATE_NARRATION");
    assert_eq!(snapshot(root.path())?, before);
    Ok(())
}

#[test]
fn image_returns_exact_saved_bytes_and_never_falls_back_from_missing_source() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path(), "exact")?;
    let roots = Roots::open(vec![root.path().to_owned()]).map_err(anyhow::Error::msg)?;
    for (selector, file) in [
        ("sheet", "contact-sheet.png"),
        ("keyframe", "frames/frame-01.png"),
    ] {
        let response = call(&roots, Tool::Image, json!({"show": "exact", selector: 1}));
        assert_eq!(content(&response)?[selector], 1);
        let image = &response["content"][1];
        assert_eq!(image["type"], "image");
        assert_eq!(image["mimeType"], "image/png");
        assert_eq!(
            STANDARD.decode(image["data"].as_str().unwrap())?,
            fs::read(directory.join(file))?
        );
    }
    let source = content(&call(
        &roots,
        Tool::Image,
        json!({"show": "exact", "at": "0.75"}),
    ))?;
    assert_eq!(source["sampleAtMs"], 100);
    assert_eq!(source["requestedAtMs"], 750);
    assert_eq!(source["annotations"], false);
    let mark = content(&call(
        &roots,
        Tool::Image,
        json!({"show": "exact", "mark": 1}),
    ))?;
    assert_eq!(mark["location"]["number"], 1);
    assert_eq!(mark["requestedAtMs"], 500);
    assert_eq!(
        call(
            &roots,
            Tool::Image,
            json!({"show": "exact", "phrase": "PRIVATE_NARRATION"})
        )["isError"],
        true
    );
    let phrase = content(&call(
        &roots,
        Tool::Image,
        json!({"show": "exact", "phrase": "PRIVATE_NARRATION", "match": 2}),
    ))?;
    assert_eq!(phrase["requestedAtMs"], 600);
    fs::remove_file(directory.join("source.frames"))?;
    assert_eq!(
        call(&roots, Tool::Image, json!({"show": "exact", "at": "0.75"}))["isError"],
        true
    );
    assert_eq!(
        call(&roots, Tool::Image, json!({"show": "exact", "sheet": 1}))["isError"],
        false
    );
    Ok(())
}

#[test]
fn truncation_is_truthful_and_untimed_or_lost_narration_is_not_timed() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path(), "exact")?;
    let roots = Roots::open(vec![root.path().to_owned()]).map_err(anyhow::Error::msg)?;
    let text = "é".repeat(TEXT_LIMIT);
    fs::write(directory.join("summary.md"), &text)?;
    let mut manifest = fixture_manifest();
    manifest["narration"] = json!({"text": text});
    fs::write(directory.join("show.json"), serde_json::to_vec(&manifest)?)?;
    let response = content(&call(
        &roots,
        Tool::Read,
        json!({"show": "exact", "narration": true}),
    ))?;
    assert_eq!(response["summary"]["truncated"], true);
    assert_eq!(response["summary"]["totalBytes"], TEXT_LIMIT * 2);
    assert_eq!(
        response["summary"]["text"].as_str().unwrap().len(),
        TEXT_LIMIT
    );
    assert_eq!(response["narration"]["kind"], "untimed");
    assert_eq!(response["narration"]["excerpt"]["truncated"], true);
    assert_eq!(
        call(&roots, Tool::Read, json!({"show": "exact", "phrase": "é"}))["isError"],
        true
    );
    manifest["narration"] = json!({"words": []});
    manifest["narrationLost"] = json!("notTranscribed");
    fs::write(directory.join("show.json"), serde_json::to_vec(&manifest)?)?;
    let response = content(&call(
        &roots,
        Tool::Read,
        json!({"show": "exact", "narration": true}),
    ))?;
    assert_eq!(response["narration"]["kind"], "lost");
    assert_eq!(response["narration"]["lost"], "notTranscribed");
    Ok(())
}

#[test]
fn invalid_selectors_unknown_properties_damage_and_oversize_do_not_leak_content()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path(), "exact")?;
    let roots = Roots::open(vec![root.path().to_owned()]).map_err(anyhow::Error::msg)?;
    for arguments in [
        json!({"show": "exact", "sheet": 0}),
        json!({"show": "exact", "sheet": 1, "keyframe": 1}),
        json!({"show": "exact", "sheet": 1, "file": "PRIVATE_PATH"}),
        json!({"show": "exact", "mark": 1, "match": 1}),
    ] {
        let response = call(&roots, Tool::Image, arguments);
        assert_eq!(response["isError"], true);
        assert!(!serde_json::to_string(&response)?.contains("PRIVATE_"));
    }
    let png = fs::read(directory.join("contact-sheet.png"))?;
    for broken in [b"\x89PNG\r\n\x1a\n".as_slice(), png.get(..33).unwrap()] {
        fs::write(directory.join("contact-sheet.png"), broken)?;
        assert_eq!(
            call(&roots, Tool::Image, json!({"show": "exact", "sheet": 1}))["isError"],
            true
        );
    }
    fs::OpenOptions::new()
        .write(true)
        .open(directory.join("contact-sheet.png"))?
        .set_len(IMAGE_LIMIT + 1)?;
    assert_eq!(
        call(&roots, Tool::Image, json!({"show": "exact", "sheet": 1}))["isError"],
        true
    );
    fs::write(directory.join("summary.md"), [0xFF, 0xFE])?;
    assert_eq!(
        call(&roots, Tool::Read, json!({"show": "exact"}))["isError"],
        true
    );
    fs::write(directory.join("show.json"), b"PRIVATE_BAD_MANIFEST")?;
    let response = call(&roots, Tool::Read, json!({"show": "exact"}));
    assert_eq!(response["isError"], true);
    assert!(!serde_json::to_string(&response)?.contains("PRIVATE_"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn linked_content_cannot_escape_an_allowed_bundle() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path(), "exact")?;
    let outside = tempfile::NamedTempFile::new()?;
    fs::write(outside.path(), b"PRIVATE_OUTSIDE")?;
    let roots = Roots::open(vec![root.path().to_owned()]).map_err(anyhow::Error::msg)?;
    for (name, tool, arguments) in [
        ("summary.md", Tool::Read, json!({"show": "exact"})),
        (
            "contact-sheet.png",
            Tool::Image,
            json!({"show": "exact", "sheet": 1}),
        ),
        (
            "source.frames",
            Tool::Image,
            json!({"show": "exact", "at": "0.5"}),
        ),
        ("show.json", Tool::Read, json!({"show": "exact"})),
    ] {
        fs::remove_file(directory.join(name))?;
        symlink(outside.path(), directory.join(name))?;
        let response = call(&roots, tool, arguments);
        assert_eq!(response["isError"], true);
        assert!(!serde_json::to_string(&response)?.contains("PRIVATE_OUTSIDE"));
    }
    Ok(())
}
