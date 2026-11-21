use std::fs;

use serde_json::json;

use super::*;

fn saved(directory: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(directory.join("frames"))?;
    fs::write(directory.join("summary.md"), b"PRIVATE_SUMMARY")?;
    fs::write(directory.join("contact-sheet.png"), b"fixture image")?;
    fs::write(directory.join("frames/frame-01.png"), b"fixture image")?;
    fs::write(
        directory.join("show.json"),
        serde_json::to_vec(&json!({
            "version": 3, "duration": 1000, "surface": {"width": 2, "height": 2},
            "strokes": [], "pins": [], "actions": [], "narration": {"text": "PRIVATE_NARRATION"},
            "marks": [], "keyframes": [{"index": 1, "at": 500, "reason": "middle", "event": "PRIVATE_EVENT", "marks": [0,0]}]
        }))?,
    )?;
    Ok(())
}

#[test]
fn inspection_json_keeps_its_published_shape_and_field_order() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = root.path().canonicalize()?;
    saved(&directory)?;
    let expected = json!({
        "id": null, "directory": directory, "status": "ok", "pinned": false,
        "expiresAt": null, "durationMs": 1000, "keyframes": 1, "marks": 0, "problems": [],
        "ending": "finished", "narrationLost": null,
        "summary": directory.join("summary.md"), "manifest": directory.join("show.json"),
        "sheets": [{"path": directory.join("contact-sheet.png"), "frames": [1]}],
        "frames": [{"index": 1, "atMs": 500, "reason": "middle", "marks": [], "path": directory.join("frames/frame-01.png")}],
        "markFrames": [], "movie": null
    });
    let mut actual = Vec::new();
    inspect(&Entry::open(&directory, None)?, true, &mut actual)?;
    let mut expected = serde_json::to_vec_pretty(&expected)?;
    expected.push(b'\n');
    assert_eq!(actual, expected);
    assert!(!String::from_utf8(actual)?.contains("PRIVATE_"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_show_folder_that_cannot_be_opened_is_listed_as_damaged_beside_intact_shows()
-> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    use crate::show::store::recall;
    let root = tempfile::tempdir()?;
    let parent = root.path().canonicalize()?;
    let [older, sealed, newer] =
        ["20261020T143205Z", "20261021T143205Z", "20261022T143205Z"].map(|name| parent.join(name));
    for show in [&older, &sealed, &newer] {
        saved(show)?;
    }
    let listing = |as_json| -> io::Result<String> {
        let mut out = Vec::new();
        list(recall::list(&parent)?, Some(&parent), as_json, &mut out)?;
        String::from_utf8(out).map_err(io::Error::other)
    };
    let access = fs::metadata(&sealed)?.permissions();
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000))?;
    let unrestricted = fs::read_dir(&sealed).is_ok();
    let listed = (listing(false), listing(true));
    fs::set_permissions(&sealed, access)?;
    if unrestricted {
        return Ok(());
    }
    let (text, json) = (listed.0?, listed.1?);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert_eq!(
        lines[0],
        format!("ok · 0:01 · 1 frames · {}", newer.display())
    );
    assert_eq!(lines[1], format!("damaged · {}", sealed.display()));
    assert!(lines[2].starts_with(&format!("Problem: {}", sealed.display())));
    assert_eq!(
        lines[3],
        format!("ok · 0:01 · 1 frames · {}", older.display())
    );
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json)?;
    assert_eq!(
        rows.iter().map(|row| &row["status"]).collect::<Vec<_>>(),
        ["ok", "damaged", "ok"]
    );
    let problems = rows[1]["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1);
    assert!(Path::new(problems[0]["file"].as_str().unwrap()).starts_with(&sealed));
    Ok(())
}
