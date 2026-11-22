use std::{
    fs,
    path::{Path, PathBuf},
};

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

fn snapshot(directory: &Path) -> anyhow::Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            found.extend(snapshot(&entry.path())?);
        } else {
            found.push((entry.path(), fs::read(entry.path())?));
        }
    }
    found.sort();
    Ok(found)
}

#[test]
fn an_unmanaged_show_with_a_timestamped_name_has_no_managed_id_or_expiry() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let managed = tempfile::tempdir()?;
    let name = "20261020T143205Z";
    fixture(root.path(), name)?;
    let roots = Roots::open(&[root.path().to_owned()], Some(managed.path().to_owned()))
        .map_err(anyhow::Error::msg)?;
    let listed = content(&call(&roots, Tool::List, json!({})))?;
    let read = content(&call(&roots, Tool::Read, json!({"show": name})))?;
    for show in [&listed["shows"][0], &read["show"]] {
        assert!(show["status"].is_string());
        assert!(show["id"].is_null());
        assert!(show["expiresAt"].is_null());
    }
    Ok(())
}

#[test]
fn reads_change_no_file_and_include_narration_only_on_request() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path(), "exact")?;
    let before = snapshot(root.path())?;
    let roots = Roots::open(&[root.path().to_owned()], None).map_err(anyhow::Error::msg)?;
    let listed = call(&roots, Tool::List, json!({}));
    assert_eq!(content(&listed)?["shows"].as_array().unwrap().len(), 1);
    let read = content(&call(&roots, Tool::Read, json!({"show": "exact"})))?;
    assert_eq!(read["summary"]["text"], "PRIVATE_EDITED_SUMMARY");
    assert!(read["narration"].is_null());
    let read = content(&call(
        &roots,
        Tool::Read,
        json!({"show": directory, "narration": true, "phrase": "PRIVATE_NARRATION"}),
    ))?;
    assert_eq!(read["narration"]["kind"], "timed");
    assert_eq!(read["location"]["matches"].as_array().unwrap().len(), 2);
    assert!(read["location"]["startMs"].is_null());
    assert_eq!(read["narration"]["rows"][0]["atMs"], 200);
    assert_eq!(
        read["narration"]["rows"][0]["text"],
        "PRIVATE_NARRATION PRIVATE_NARRATION"
    );
    assert_eq!(snapshot(root.path())?, before);
    Ok(())
}

#[test]
fn image_returns_exact_saved_bytes_and_never_falls_back_from_missing_source() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path(), "exact")?;
    let roots = Roots::open(&[root.path().to_owned()], None).map_err(anyhow::Error::msg)?;
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
    assert_eq!(source["location"]["kind"], "time");
    assert_eq!(source["sampledMs"], 100);
    assert_eq!(source["requestedMs"], 750);
    assert_eq!(source["annotations"], false);
    let mark = content(&call(
        &roots,
        Tool::Image,
        json!({"show": "exact", "mark": 1}),
    ))?;
    assert_eq!(mark["location"]["kind"], "mark");
    assert_eq!(mark["location"]["number"], 1);
    assert_eq!(mark["requestedMs"], 500);
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
    assert_eq!(phrase["location"]["kind"], "phrase");
    assert_eq!(phrase["location"]["match"], 2);
    assert_eq!(phrase["requestedMs"], 600);
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
fn a_long_summary_is_truncated_truthfully_and_narration_arrives_whole() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path(), "exact")?;
    let roots = Roots::open(&[root.path().to_owned()], None).map_err(anyhow::Error::msg)?;
    let text = "é".repeat(TEXT_LIMIT);
    fs::write(directory.join("summary.md"), &text)?;
    let mut manifest = fixture_manifest();
    let words: Vec<_> = (0..800_u64)
        .map(|index| json!({"span": [index * 300, index * 300 + 200], "text": format!("w{index}")}))
        .collect();
    manifest["duration"] = json!(240_000);
    manifest["narration"] = json!({"words": words});
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
    let rows = response["narration"]["rows"].as_array().unwrap();
    let spoken: Vec<_> = rows
        .iter()
        .flat_map(|row| row["text"].as_str().unwrap().split_whitespace())
        .collect();
    assert_eq!(spoken.len(), 800);
    assert_eq!(spoken.last(), Some(&"w799"));
    manifest["narration"] = json!({"text": text});
    fs::write(directory.join("show.json"), serde_json::to_vec(&manifest)?)?;
    let response = content(&call(
        &roots,
        Tool::Read,
        json!({"show": "exact", "narration": true}),
    ))?;
    assert_eq!(response["narration"]["kind"], "untimed");
    assert_eq!(response["narration"]["text"], text);
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
fn a_tool_error_names_what_went_wrong() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path(), "exact")?;
    let roots = Roots::open(&[root.path().to_owned()], None).map_err(anyhow::Error::msg)?;
    let message = |tool: Tool, arguments: Value| {
        let result = call(&roots, tool, arguments);
        assert_eq!(result["isError"], true, "{result}");
        result["content"][0]["text"].as_str().unwrap().to_owned()
    };
    let unknown = message(Tool::Read, json!({"show": "exact", "bogus": 1}));
    assert!(unknown.contains("bogus"), "{unknown}");
    let missing = message(Tool::Read, json!({"show": "elsewhere"}));
    let configured = root.path().canonicalize()?.display().to_string();
    assert!(
        missing.contains("elsewhere") && missing.contains(&configured),
        "{missing}"
    );
    let time = message(Tool::Read, json!({"show": "exact", "at": "1:75"}));
    assert!(time.contains("less than 60"), "{time}");
    fs::remove_file(directory.join("contact-sheet.png"))?;
    let sheet = message(Tool::Image, json!({"show": "exact", "sheet": 1}));
    assert!(sheet.contains("contact-sheet.png"), "{sheet}");
    Ok(())
}

#[test]
fn a_selector_other_than_exactly_one_picture_is_a_tool_error() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    fixture(root.path(), "exact")?;
    let roots = Roots::open(&[root.path().to_owned()], None).map_err(anyhow::Error::msg)?;
    for arguments in [
        json!({"show": "exact", "sheet": 0}),
        json!({"show": "exact", "sheet": 1, "keyframe": 1}),
        json!({"show": "exact", "sheet": 1, "file": "contact-sheet.png"}),
        json!({"show": "exact", "mark": 1, "match": 1}),
    ] {
        assert_eq!(call(&roots, Tool::Image, arguments)["isError"], true);
    }
    Ok(())
}
