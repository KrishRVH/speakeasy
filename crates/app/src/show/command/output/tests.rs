use std::fs;

use serde_json::json;

use super::*;

#[test]
fn inspection_json_keeps_its_published_shape_and_field_order() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = root.path().canonicalize()?;
    fs::create_dir(directory.join("frames"))?;
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
    inspect(Entry::open(&directory)?, None, true, &mut actual)?;
    let mut expected = serde_json::to_vec_pretty(&expected)?;
    expected.push(b'\n');
    assert_eq!(actual, expected);
    assert!(!String::from_utf8(actual)?.contains("PRIVATE_"));
    Ok(())
}
